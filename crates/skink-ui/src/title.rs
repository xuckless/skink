//! The outer terminal's window/tab title: the active tab and the project,
//! like `main.rs — skink`, instead of the command line (`skink .`).
//! The terminal's own title is saved (xterm title stack) and given back on
//! exit.

use std::io::{self, Write};
use std::path::PathBuf;

use crate::{App, Tab};

/// The project name for the title, kept until the live file changes (the
/// root lookup touches the disk).
#[derive(Default)]
pub(crate) enum ProjectName {
    #[default]
    Unknown,
    For {
        file: Option<PathBuf>,
        name: String,
    },
}

/// What the terminal shows now, so it is only written when it changes.
#[derive(Default)]
pub(crate) enum Shown {
    #[default]
    Nothing,
    Title(String),
}

#[derive(Default)]
pub(crate) struct WindowTitle {
    project: ProjectName,
    shown: Shown,
}

impl App {
    /// `file — project` (`●` first when the file has unsaved changes).
    pub(crate) fn window_title(&mut self) -> String {
        let project = self.title_project();
        let tab = match self.tabs.get(self.active) {
            Some(Tab::Term(p)) => p.title().to_owned(),
            // The empty startup tab: just the project.
            _ if self.active == self.cur_doc && self.live_is_pristine() => String::new(),
            _ => {
                let d = self.doc_at(self.active).unwrap_or(&self.doc);
                let dirty = if d.editor.buffer.is_dirty() { "● " } else { "" };
                format!("{dirty}{}", d.title())
            }
        };
        if tab.is_empty() || tab == project {
            project
        } else {
            format!("{tab} — {project}")
        }
    }

    fn title_project(&mut self) -> String {
        if let Some(t) = self.tree() {
            return dir_name(&t.root);
        }
        let file = self.doc.canon.clone();
        if let ProjectName::For { file: f, name } = &self.title.project
            && *f == file
        {
            return name.clone();
        }
        let name = dir_name(&self.project_root());
        self.title.project = ProjectName::For {
            file,
            name: name.clone(),
        };
        name
    }

    /// Write the title if it changed since the last frame.
    pub(crate) fn update_window_title(&mut self, out: &mut impl Write) -> io::Result<()> {
        let want = sanitize(&self.window_title());
        if matches!(&self.title.shown, Shown::Title(t) if *t == want) {
            return Ok(());
        }
        write!(out, "\x1b]2;{want}\x07")?;
        out.flush()?;
        self.title.shown = Shown::Title(want);
        Ok(())
    }
}

fn dir_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// No control bytes: a file name must not end the OSC early.
fn sanitize(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Save the terminal's title (CSI 22 t) before Skink sets its own.
pub(crate) fn push(out: &mut impl Write) {
    let _ = write!(out, "\x1b[22;0t");
    let _ = out.flush();
}

/// Give the terminal its saved title back (CSI 23 t).
pub(crate) fn pop(out: &mut impl Write) {
    let _ = write!(out, "\x1b[23;0t");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_bytes_cannot_end_the_title_sequence() {
        assert_eq!(sanitize("a\x07b\x1b]0;evil"), "ab]0;evil");
    }
}
