//! Docks: a terminal along the bottom and the agent panel on the right,
//! as panes at the edge of the pane tree.

use crate::panes::{Axis, PaneId};
use crate::{App, Tab};
use skink_term::CommandSpec;

/// One dock's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dock {
    #[default]
    Closed,
    Open(PaneId),
}

#[derive(Debug, Default)]
pub struct Docks {
    pub bottom: Dock,
    pub right: Dock,
}

/// Share (‰) the editor panes keep.
const BOTTOM_SHARE: u16 = 700;
const RIGHT_SHARE: u16 = 640;

impl App {
    /// The dock's pane if it is still there (Alt-Q may have closed it).
    fn dock_pane(&self, d: Dock) -> Option<PaneId> {
        match d {
            Dock::Open(id) if self.panes.pane(id).is_some() => Some(id),
            _ => None,
        }
    }

    fn close_dock(&mut self, id: PaneId) {
        if let Some((tab, view)) = self.panes.close(id) {
            self.switch_to(tab);
            if let crate::panes::PaneView::Remembered(v) = view
                && !self.active_is_term()
            {
                self.doc.editor.set_view(v);
            }
        }
    }

    /// Alt-T: a shell along the bottom; again closes it (the shell keeps
    /// running in its tab).
    pub fn toggle_terminal_dock(&mut self) {
        if let Some(id) = self.dock_pane(self.docks.bottom) {
            self.docks.bottom = Dock::Closed;
            return self.close_dock(id);
        }
        // The newest shell tab no other pane shows, else a new one.
        let shown = |a: &App, i: usize| {
            a.panes.others_showing(i) || a.panes.pane(a.panes.focused()).is_some_and(|p| p.tab == i)
        };
        let existing = (0..self.tabs.len()).rev().find(|&i| {
            matches!(&self.tabs[i], Tab::Term(p) if p.shell_cwd.is_some()) && !shown(self, i)
        });
        let tab = match existing {
            Some(i) => i,
            None => match self.spawn_terminal_tab(CommandSpec::default_shell()) {
                Ok(i) => i,
                Err(e) => {
                    self.doc.editor.message = Some(format!("terminal: {e}"));
                    return;
                }
            },
        };
        let id = self.panes.dock(Axis::Rows, BOTTOM_SHARE, tab);
        self.docks.bottom = Dock::Open(id);
        self.focus_pane(id);
    }

    /// Alt-Shift-T: the agent panel on the right; again closes it.
    pub fn toggle_agent_dock(&mut self) {
        if let Some(id) = self.dock_pane(self.docks.right) {
            self.docks.right = Dock::Closed;
            return self.close_dock(id);
        }
        let here = self.cur_doc;
        let id = self.panes.dock(Axis::Columns, RIGHT_SHARE, here);
        self.docks.right = Dock::Open(id);
        self.focus_pane(id);
        self.open_agent_panel("");
    }
}
