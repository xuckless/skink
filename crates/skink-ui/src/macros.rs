//! Keyboard macros (Micro's ToggleMacro Ctrl-U / PlayMacro Ctrl-J): record
//! the keys pressed, replay them through the normal key handling.

use crate::App;
use ratatui::crossterm::event::KeyEvent;

/// Longest macro kept (keys).
const MAX_KEYS: usize = 10_000;

#[derive(Debug, Default)]
pub enum MacroState {
    #[default]
    Idle,
    Recording(Vec<KeyEvent>),
    /// Replaying: keys are not recorded and PlayMacro does not nest.
    Playing,
}

impl App {
    /// Is `ev` bound to one of the macro actions themselves?
    fn is_macro_key(&self, ev: KeyEvent) -> bool {
        crate::input::key_spec(ev)
            .and_then(|s| self.keymap.command(&s).map(str::to_owned))
            .is_some_and(|c| {
                c.split(['|', ','])
                    .any(|n| matches!(n.trim(), "ToggleMacro" | "PlayMacro"))
            })
    }

    pub(crate) fn record_key(&mut self, ev: KeyEvent) {
        if !matches!(self.macro_state, MacroState::Recording(_)) || self.is_macro_key(ev) {
            return;
        }
        if let MacroState::Recording(keys) = &mut self.macro_state
            && keys.len() < MAX_KEYS
        {
            keys.push(ev);
        }
    }

    /// Ctrl-U: start recording, or stop and keep what was recorded.
    pub fn toggle_macro(&mut self) {
        match std::mem::take(&mut self.macro_state) {
            MacroState::Idle => {
                self.macro_state = MacroState::Recording(Vec::new());
                self.doc.editor.message = Some("Recording macro (Ctrl-U stops)".into());
            }
            MacroState::Recording(keys) => {
                self.doc.editor.message = Some(format!(
                    "Macro recorded: {} keys (Ctrl-J plays)",
                    keys.len()
                ));
                self.last_macro = keys;
            }
            MacroState::Playing => self.macro_state = MacroState::Playing,
        }
    }

    /// Ctrl-J: replay the last macro.
    pub fn play_macro(&mut self) {
        match self.macro_state {
            MacroState::Idle => {}
            MacroState::Recording(_) => {
                self.doc.editor.message = Some("Stop recording first (Ctrl-U)".into());
                return;
            }
            MacroState::Playing => return,
        }
        if self.last_macro.is_empty() {
            self.doc.editor.message = Some("No macro recorded (Ctrl-U records)".into());
            return;
        }
        self.macro_state = MacroState::Playing;
        let keys = self.last_macro.clone();
        for ev in &keys {
            self.handle_key(*ev);
        }
        self.macro_state = MacroState::Idle;
        // A toast: the status line keeps whatever the macro itself said.
        self.toast(
            crate::nav::Level::Info,
            format!("Played macro ({} keys)", keys.len()),
        );
    }

    /// Recording shows in the status bar.
    pub(crate) fn macro_recording(&self) -> bool {
        matches!(self.macro_state, MacroState::Recording(_))
    }
}
