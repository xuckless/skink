//! Text zoom. A terminal app cannot set its own font size, so Skink does
//! what is possible:
//! - zoom pane (Alt-Z): hide the tab bar, tree and status line;
//! - font size through whoever owns the terminal: EveryAI (`ui.font_size`
//!   handover tool), Kitty remote control (`kitten @ set-font-size`), else
//!   a hint to use the terminal's own zoom keys (left unbound in Skink).

use crate::App;
use skink_host::tools::FontStep;
use std::sync::mpsc;

/// What the window shows around the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Normal,
    /// Only the main area (the status line returns while a prompt or a
    /// message is up).
    Zoomed,
}

/// Who can change the terminal's font size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontControl {
    EveryAI,
    Kitty,
    /// Only the user, with the terminal's own keys.
    Terminal,
}

impl FontControl {
    pub fn detect() -> Self {
        if skink_host::HostEndpoint::from_env().is_some() {
            FontControl::EveryAI
        } else if std::env::var_os("KITTY_WINDOW_ID").is_some() {
            FontControl::Kitty
        } else {
            FontControl::Terminal
        }
    }
}

/// A font change running in the background.
pub enum FontJob {
    Idle,
    Running(mpsc::Receiver<Result<String, String>>),
}

fn change(control: FontControl, step: FontStep) -> Result<String, String> {
    match control {
        FontControl::EveryAI => {
            let client = skink_host::connect_if_present().ok_or("EveryAI host not reachable")?;
            let r: skink_host::tools::FontSizeResult = client
                .call_typed(
                    skink_host::tools::UI_FONT_SIZE,
                    &skink_host::tools::FontSizeArgs { step },
                )
                .map_err(|e| e.to_string())?;
            Ok(format!("font size {}", r.size))
        }
        FontControl::Kitty => {
            let arg = match step {
                FontStep::Bigger => "+1",
                FontStep::Smaller => "-1",
                FontStep::Reset => "0",
            };
            let out = std::process::Command::new("kitten")
                .args(["@", "set-font-size", "--", arg])
                .output()
                .map_err(|e| format!("kitten: {e}"))?;
            if out.status.success() {
                Ok("font size changed".into())
            } else {
                Err("kitty refused (set allow_remote_control in kitty.conf)".into())
            }
        }
        FontControl::Terminal => Err(
            "use your terminal's zoom keys (e.g. Ctrl-= / Ctrl--); Skink leaves them free".into(),
        ),
    }
}

impl App {
    /// Alt-Z.
    pub(crate) fn toggle_zoom(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Normal => ViewMode::Zoomed,
            ViewMode::Zoomed => ViewMode::Normal,
        };
    }

    /// `:fontsize +1|-1|0`, FontBigger / FontSmaller / FontReset.
    pub(crate) fn change_font(&mut self, step: FontStep) {
        let control = FontControl::detect();
        if control == FontControl::Terminal {
            self.doc.editor.message = change(control, step).err();
            return;
        }
        let (tx, rx) = mpsc::channel();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let _ = tx.send(change(control, step));
            if let Some(w) = wake {
                w();
            }
        });
        self.font_job = FontJob::Running(rx);
    }

    pub(crate) fn poll_font(&mut self) {
        let FontJob::Running(rx) = &self.font_job else {
            return;
        };
        let msg = match rx.try_recv() {
            Err(mpsc::TryRecvError::Empty) => return,
            Ok(Ok(m)) => m,
            Ok(Err(e)) => format!("font size: {e}"),
            Err(mpsc::TryRecvError::Disconnected) => "font size: worker died".into(),
        };
        self.doc.editor.message = Some(msg);
        self.font_job = FontJob::Idle;
    }
}
