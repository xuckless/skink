//! Micro-compatible config: key names, default keybindings (settings and
//! colorschemes follow in Phase 1). No terminal-library dependency.

mod remotes;
pub mod schema;
mod settings;
pub use remotes::{AutoForward, ForwardSpec, Remote};
pub use settings::{
    AgentChoice, AiMode, AiSettings, DEFAULT_BINDINGS_FILE, DEFAULT_SETTINGS, EffortChoice,
    LspSetting, MacOption, ModelChoice, Settings, UpdatePolicy, config_dir, parse_bindings,
    project_settings_path, recent_load, recent_push, relaxed_json, state_dir,
};

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    Insert,
    BackTab,
    /// Function key `F(1)`..=`F(20)`.
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

impl std::fmt::Display for KeySpec {
    /// Readable name, e.g. `Ctrl-S`, `Alt-Shift-G`, `Ctrl-Space`, `Shift-F6`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl-")?;
        }
        if self.alt {
            f.write_str("Alt-")?;
        }
        if self.shift {
            f.write_str("Shift-")?;
        }
        match self.key {
            Key::Char(' ') => f.write_str("Space"),
            Key::Char(c) if self.ctrl || self.alt => write!(f, "{}", c.to_ascii_uppercase()),
            Key::Char(c) => write!(f, "{c}"),
            Key::F(n) => write!(f, "F{n}"),
            Key::BackTab => f.write_str("Shift-Tab"),
            k => write!(f, "{k:?}"),
        }
    }
}

/// Parse a Micro key name such as `Ctrl-q`, `CtrlQ`, `Alt-x`, `Shift-Up`,
/// `CtrlHome` or `PageDown`.
pub fn parse_key(name: &str) -> Option<KeySpec> {
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let mut rest = name;
    loop {
        if let Some(r) = rest.strip_prefix("Ctrl") {
            ctrl = true;
            rest = r.strip_prefix('-').unwrap_or(r);
        } else if let Some(r) = rest.strip_prefix("Alt") {
            alt = true;
            rest = r.strip_prefix('-').unwrap_or(r);
        } else if let Some(r) = rest.strip_prefix("Shift") {
            shift = true;
            rest = r.strip_prefix('-').unwrap_or(r);
        } else {
            break;
        }
    }
    let key = match rest {
        "Up" => Key::Up,
        "Down" => Key::Down,
        "Left" => Key::Left,
        "Right" => Key::Right,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "Enter" => Key::Enter,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "Escape" | "Esc" => Key::Escape,
        "Space" => Key::Char(' '),
        "Insert" => Key::Insert,
        "Backtab" | "BackTab" => Key::BackTab,
        f if f.len() >= 2
            && f.starts_with('F')
            && f[1..].bytes().all(|b| b.is_ascii_digit())
            && f[1..].parse::<u8>().is_ok_and(|n| (1..=20).contains(&n)) =>
        {
            Key::F(f[1..].parse().unwrap_or(1))
        }
        s => {
            let mut it = s.chars();
            let c = it.next()?;
            if it.next().is_some() {
                return None;
            }
            if alt && !ctrl && c.is_ascii_uppercase() {
                shift = true;
            }
            Key::Char(if ctrl || alt {
                c.to_ascii_lowercase()
            } else {
                c
            })
        }
    };
    Some(KeySpec {
        ctrl,
        alt,
        shift,
        key,
    })
}

// Adapted from zyedidia/micro internal/action/defaults_other.go @ c9ba51d68e073c55b1c8c1e892a8c8abf025f38f,
// MIT, Copyright (c) 2016-2020 Zachary Yedidia, et al.
// Changes: mouse bindings and a few Micro-only commands dropped (Skink handles
// the mouse directly); table form instead of a Go map.
pub const DEFAULT_BINDINGS: &[(&str, &str)] = &[
    ("Up", "CursorUp"),
    ("Down", "CursorDown"),
    ("Right", "CursorRight"),
    ("Left", "CursorLeft"),
    ("ShiftUp", "SelectUp"),
    ("ShiftDown", "SelectDown"),
    ("ShiftLeft", "SelectLeft"),
    ("ShiftRight", "SelectRight"),
    ("CtrlLeft", "WordLeft"),
    ("CtrlRight", "WordRight"),
    ("AltUp", "MoveLinesUp"),
    ("AltDown", "MoveLinesDown"),
    ("CtrlShiftRight", "SelectWordRight"),
    ("CtrlShiftLeft", "SelectWordLeft"),
    ("AltLeft", "StartOfTextToggle"),
    ("AltRight", "EndOfLine"),
    ("AltShiftLeft", "SelectToStartOfTextToggle"),
    ("ShiftHome", "SelectToStartOfTextToggle"),
    ("AltShiftRight", "SelectToEndOfLine"),
    ("ShiftEnd", "SelectToEndOfLine"),
    ("CtrlUp", "CursorStart"),
    ("CtrlDown", "CursorEnd"),
    ("CtrlShiftUp", "SelectToStart"),
    ("CtrlShiftDown", "SelectToEnd"),
    ("Enter", "InsertNewline"),
    ("CtrlH", "Backspace"),
    ("Backspace", "Backspace"),
    ("Alt-CtrlH", "DeleteWordLeft"),
    ("Alt-Backspace", "DeleteWordLeft"),
    ("Tab", "Autocomplete|IndentSelection|InsertTab"),
    (
        "Backtab",
        "CycleAutocompleteBack|OutdentSelection|OutdentLine",
    ),
    ("Ctrl-s", "Save"),
    ("Ctrl-f", "Find"),
    ("Ctrl-n", "FindNext"),
    ("Ctrl-p", "FindPrevious"),
    ("Ctrl-z", "Undo"),
    ("Ctrl-y", "Redo"),
    ("Ctrl-c", "Copy|CopyLine"),
    ("Ctrl-x", "Cut|CutLine"),
    ("Ctrl-k", "CutLine"),
    // Skink: Ctrl-D adds the next match as a cursor (VS Code / Sublime);
    // duplicate moves to Ctrl-Shift-D (needs the kitty keyboard protocol).
    ("Ctrl-d", "SpawnMultiCursor"),
    ("CtrlShift-d", "Duplicate|DuplicateLine"),
    ("Ctrl-v", "Paste"),
    ("Ctrl-a", "SelectAll"),
    ("Home", "StartOfTextToggle"),
    ("End", "EndOfLine"),
    ("CtrlHome", "CursorStart"),
    ("CtrlEnd", "CursorEnd"),
    ("CtrlShiftHome", "SelectToStart"),
    ("CtrlShiftEnd", "SelectToEnd"),
    ("PageUp", "CursorPageUp"),
    ("PageDown", "CursorPageDown"),
    ("ShiftPageUp", "SelectPageUp"),
    ("ShiftPageDown", "SelectPageDown"),
    ("Delete", "Delete"),
    ("Ctrl-q", "Quit"),
    ("Ctrl-l", "command-edit:goto "),
    ("Ctrl-e", "command-edit:"),
    ("Alt-p", "QuickOpen"),
    ("Alt-,", "PreviousTab"),
    ("Alt-.", "NextTab"),
    ("Ctrl-t", "AddTab"),
    ("Ctrl-o", "OpenFile"),
    // Tabs, terminals and the tree. F-keys and Ctrl-PageUp/Down bound to app
    // actions also work while a terminal tab has focus.
    ("CtrlPageUp", "PreviousTab"),
    ("CtrlPageDown", "NextTab"),
    ("F8", "ToggleTree"),
    ("Ctrl-b", "ToggleSidebar"),
    ("Ctrl-]", "JumpToMatchingBrace"),
    ("F11", "DebugStepInto|CloseTerminal"),
    ("F12", "ToggleTerminal"),
    ("ShiftF12", "NewTerminal"),
    ("Ctrl-u", "ToggleMacro"),
    ("Ctrl-j", "PlayMacro"),
    ("ShiftF10", "RunTask"),
    ("ShiftF9", "RerunTask"),
    ("ShiftF11", "DebugStepOut|TerminalHistory"),
    ("ShiftF4", "SelectLastOutput"),
    ("ShiftF2", "PreviousPrompt"),
    ("ShiftF3", "NextPrompt"),
    // Emacs-style
    ("Alt-f", "WordRight"),
    ("Alt-b", "WordLeft"),
    ("Alt-a", "StartOfText"),
    ("Alt-e", "EndOfLine"),
    // Integration with file managers
    ("F2", "Save"),
    ("F3", "Find"),
    ("F4", "Quit"),
    ("F7", "Find"),
    // Debugging (VS Code's keys). The step keys fall through to their
    // other action when no debug session is live.
    ("F10", "DebugStepOver|Quit"),
    ("Esc", "Escape,RemoveAllMultiCursors|Deselect,ClearInfo"),
    // Multiple cursors (Micro; Alt-p stays QuickOpen)
    ("Alt-n", "SpawnMultiCursor"),
    ("Alt-o", "ExpandSelection"),
    ("Alt-z", "ToggleZoom"),
    ("Alt-v", "TogglePreview"),
    ("Alt-i", "ShrinkSelection"),
    ("AltShiftUp", "SpawnMultiCursorUp"),
    ("AltShiftDown", "SpawnMultiCursorDown"),
    ("Alt-CtrlUp", "SpawnMultiCursorUp"),
    ("Alt-CtrlDown", "SpawnMultiCursorDown"),
    ("Alt-c", "RemoveAllMultiCursors"),
    // Comments (Micro's comment plugin uses Alt-/ and Ctrl-_; legacy
    // terminals report Ctrl-/ as Ctrl-7)
    ("Alt-/", "ToggleComment"),
    ("Ctrl-/", "ToggleComment"),
    ("Ctrl-7", "ToggleComment"),
    ("Alt-x", "SkipMultiCursor"),
    // Skink additions (not in Micro)
    ("Ctrl-Delete", "DeleteWordRight"),
    ("Alt-d", "DeleteWordRight"),
    ("Alt-h", "ToggleHiddenFiles"),
    // Code intelligence (language servers).
    ("Ctrl-Space", "Complete"),
    ("Alt-k", "Hover"),
    // Rebase list (git-rebase-todo): pick → reword → edit → squash → ...
    ("Alt-K", "CycleRebaseVerb"),
    ("Alt-g", "GotoDefinition"),
    ("Alt-G", "GotoTypeDefinition"),
    ("Alt-j", "PeekDefinition"),
    ("Alt-r", "FindReferences"),
    ("Alt-Enter", "CodeActions"),
    ("Alt-F", "Format"),
    ("Ctrl-r", "Outline"),
    ("Alt-w", "WorkspaceSymbols"),
    ("F1", "CommandPalette"),
    ("F9", "ToggleBreakpoint"),
    ("Alt-F9", "EditBreakpoint"),
    ("ShiftF8", "Problems"),
    ("Ctrl-g", "AiPrompt"),
    ("Alt-A", "AgentPanel"),
    ("Alt-E", "AgentSendErrors"),
    ("Alt-R", "RenameSymbol"),
    ("F6", "DebugPause|NextDiagnostic"),
    ("ShiftF6", "PreviousDiagnostic"),
    // F5 debugs (start / continue); zoom stays on Alt-Z (macOS Option is
    // translated). Shift-F5 stops.
    ("F5", "DebugContinue"),
    ("ShiftF5", "DebugStop"),
    ("Alt-l", "DebugPanel"),
    // Split panes (Micro's action names; Ctrl-W as in Micro). Legacy
    // terminals send Ctrl-\\ as Ctrl-4.
    ("Ctrl-\\", "VSplit"),
    ("Ctrl-4", "VSplit"),
    ("Alt-\\", "HSplit"),
    ("Ctrl-w", "NextSplit"),
    // Jump list.
    ("Alt-u", "JumpBack"),
    ("Alt-U", "JumpForward"),
    // Docks.
    ("Alt-t", "ToggleTerminalDock"),
    ("Alt-T", "ToggleAgentDock"),
    // Code folding.
    ("Alt-m", "ToggleFold"),
    ("Alt-M", "UnfoldAll"),
    // Project-wide search.
    ("Alt-s", "FindInFiles"),
    ("CtrlShift-f", "FindInFiles"),
    ("Alt-W", "PreviousSplit"),
    ("Alt-q", "Unsplit"),
    ("Alt-=", "GrowSplit"),
    ("Alt-N", "NextHunk"),
    ("Alt-P", "PreviousHunk"),
    ("Alt-D", "PeekHunk"),
    ("Alt-S", "StageHunk"),
    ("Alt-B", "ToggleInlineBlame"),
    ("Alt-C", "GitPanel"),
    ("Alt--", "ShrinkSplit"),
];

/// A keymap preset (`"keymap"` in settings.json): Micro's bindings (the
/// default) or those with the keys VS Code or Sublime Text users reach for.
/// bindings.json applies on top of whichever is chosen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeymapPreset {
    #[default]
    Micro,
    Vscode,
    Sublime,
}

/// VS Code keys over the Micro defaults. Ctrl-E (the command line) stays:
/// everything else is reachable from it.
pub const VSCODE_BINDINGS: &[(&str, &str)] = &[
    ("Ctrl-p", "QuickOpen"),
    ("CtrlShift-p", "CommandPalette"),
    ("Ctrl-g", "command-edit:goto "),
    ("CtrlShift-i", "AiPrompt"),
    ("Ctrl-t", "WorkspaceSymbols"),
    ("CtrlShift-o", "Outline"),
    ("Ctrl-n", "AddTab"),
    ("Ctrl-w", "Quit"),
    ("F3", "FindNext"),
    ("ShiftF3", "FindPrevious"),
    ("F12", "GotoDefinition"),
    ("ShiftF12", "FindReferences"),
    ("F2", "RenameSymbol"),
    ("F8", "NextDiagnostic"),
    ("ShiftF8", "PreviousDiagnostic"),
    ("Ctrl-.", "CodeActions"),
    ("Ctrl-`", "ToggleTerminal"),
    ("AltShiftUp", "Duplicate|DuplicateLine"),
    ("AltShiftDown", "Duplicate|DuplicateLine"),
    ("CtrlShift-z", "Redo"),
    ("CtrlShift-e", "ToggleTree"),
];

/// Sublime Text keys over the Micro defaults.
pub const SUBLIME_BINDINGS: &[(&str, &str)] = &[
    ("Ctrl-p", "QuickOpen"),
    ("CtrlShift-p", "CommandPalette"),
    ("Ctrl-g", "command-edit:goto "),
    ("CtrlShift-i", "AiPrompt"),
    ("Ctrl-n", "AddTab"),
    ("Ctrl-w", "Quit"),
    ("Ctrl-b", "RunTask"),
    ("CtrlShift-b", "RerunTask"),
    ("F3", "FindNext"),
    ("ShiftF3", "FindPrevious"),
    ("F12", "GotoDefinition"),
    ("CtrlShiftUp", "MoveLinesUp"),
    ("CtrlShiftDown", "MoveLinesDown"),
    ("Ctrl-`", "ToggleTerminal"),
    ("CtrlShift-z", "Redo"),
];

impl KeymapPreset {
    pub fn name(self) -> &'static str {
        match self {
            KeymapPreset::Micro => "micro",
            KeymapPreset::Vscode => "vscode",
            KeymapPreset::Sublime => "sublime",
        }
    }

    pub fn from_name(n: &str) -> Option<Self> {
        match n {
            "micro" => Some(KeymapPreset::Micro),
            "vscode" => Some(KeymapPreset::Vscode),
            "sublime" => Some(KeymapPreset::Sublime),
            _ => None,
        }
    }

    /// What the preset changes in the Micro defaults.
    pub fn overrides(self) -> &'static [(&'static str, &'static str)] {
        match self {
            KeymapPreset::Micro => &[],
            KeymapPreset::Vscode => VSCODE_BINDINGS,
            KeymapPreset::Sublime => SUBLIME_BINDINGS,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Keymap {
    map: HashMap<KeySpec, String>,
}

impl Keymap {
    pub fn bind(&mut self, key: &str, command: &str) -> bool {
        match parse_key(key) {
            Some(k) => {
                self.map.insert(k, command.to_owned());
                true
            }
            None => false,
        }
    }

    /// Apply a user `bindings.json`; returns the keys that did not parse.
    pub fn apply_user(&mut self, pairs: &[(String, String)]) -> Vec<String> {
        pairs
            .iter()
            .filter(|(k, a)| !self.bind(k, a))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Every binding as (readable key, action chain), sorted by key name.
    pub fn bindings(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .map
            .iter()
            .map(|(k, c)| (k.to_string(), c.clone()))
            .collect();
        v.sort();
        v
    }

    /// Put a preset's keys over this keymap (bad keys are a bug: tested).
    pub fn apply_preset(&mut self, p: KeymapPreset) {
        for (k, c) in p.overrides() {
            self.bind(k, c);
        }
    }

    pub fn command(&self, key: &KeySpec) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn defaults() -> Self {
        let mut m = Self::default();
        for (k, c) in DEFAULT_BINDINGS {
            assert!(m.bind(k, c), "bad default binding {k}");
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_micro_names() {
        let q = KeySpec {
            ctrl: true,
            alt: false,
            shift: false,
            key: Key::Char('q'),
        };
        assert_eq!(parse_key("Ctrl-q"), Some(q));
        assert_eq!(parse_key("CtrlQ"), Some(q));
        assert_eq!(
            parse_key("CtrlHome").map(|k| (k.ctrl, k.key)),
            Some((true, Key::Home))
        );
        assert_eq!(
            parse_key("Shift-Up").map(|k| (k.shift, k.key)),
            Some((true, Key::Up))
        );
        assert_eq!(
            parse_key("Alt-/").map(|k| (k.alt, k.key)),
            Some((true, Key::Char('/')))
        );
        assert_eq!(parse_key("Bogus"), None);
    }

    #[test]
    fn default_bindings_are_unique() {
        // Regression: Alt-. was bound to both NextTab and ToggleHiddenFiles,
        // silently disabling the first.
        let mut seen = std::collections::HashSet::new();
        for (k, _) in DEFAULT_BINDINGS {
            assert!(seen.insert(parse_key(k).unwrap()), "{k} bound twice");
        }
    }

    /// Each preset: every key parses, none is listed twice, and none
    /// restates a Micro default (that would hide what the preset changes).
    #[test]
    fn preset_bindings_are_unique_and_change_something() {
        let defaults = Keymap::defaults();
        for p in [KeymapPreset::Vscode, KeymapPreset::Sublime] {
            let mut seen = std::collections::HashSet::new();
            for (k, c) in p.overrides() {
                let key =
                    parse_key(k).unwrap_or_else(|| panic!("{}: {k} does not parse", p.name()));
                assert!(seen.insert(key), "{}: {k} bound twice", p.name());
                assert_ne!(
                    defaults.command(&key),
                    Some(*c),
                    "{}: {k} is already {c}",
                    p.name()
                );
            }
            let mut m = Keymap::defaults();
            m.apply_preset(p);
            assert_eq!(m.command(&parse_key("Ctrl-p").unwrap()), Some("QuickOpen"));
            assert_eq!(KeymapPreset::from_name(p.name()), Some(p));
        }
    }

    #[test]
    fn defaults_load() {
        let m = Keymap::defaults();
        let q = parse_key("Ctrl-q").unwrap();
        assert_eq!(m.command(&q), Some("Quit"));
        let c = parse_key("Ctrl-c").unwrap();
        assert_eq!(m.command(&c), Some("Copy|CopyLine"));
    }

    #[test]
    fn micro_names_with_modifiers() {
        let k = parse_key("CtrlShiftRight").unwrap();
        assert!(k.ctrl && k.shift && k.key == Key::Right);
        let k = parse_key("Alt-CtrlH").unwrap();
        assert!(k.alt && k.ctrl && k.key == Key::Char('h'));
        assert_eq!(parse_key("Backtab").unwrap().key, Key::BackTab);
        assert_eq!(parse_key("F10").unwrap().key, Key::F(10));
        let sf12 = parse_key("ShiftF12").unwrap();
        assert!(sf12.shift && sf12.key == Key::F(12));
        let cpu = parse_key("CtrlPageUp").unwrap();
        assert!(cpu.ctrl && cpu.key == Key::PageUp);
        assert_eq!(parse_key("Esc").unwrap().key, Key::Escape);
        assert!(parse_key("Alt-F").unwrap().shift);
    }
}
