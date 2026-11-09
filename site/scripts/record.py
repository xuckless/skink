#!/usr/bin/env python3
"""Record real Skink sessions as asciicast v2 files for the docs site.

    python3 site/scripts/record.py [--skink PATH] [demo ...]

Each demo runs the real binary in a pty with a throwaway HOME (so no local
paths leak), a copy of scripts/demo-project as a git repo, and scripted keys.
Captions become asciicast markers; the site shows them as a key overlay.
Linux/macOS only. Writes site/public/casts/<demo>.cast.
"""
import argparse
import json
import os
import pty
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import fcntl
import termios

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "public", "casts")
W, H = 110, 30

ESC = "\x1b"
K = {
    "enter": "\r", "tab": "\t", "esc": ESC, "bs": "\x7f", "space": " ",
    "up": ESC + "[A", "down": ESC + "[B", "right": ESC + "[C", "left": ESC + "[D",
    "f1": ESC + "OP", "f5": ESC + "[15~", "f8": ESC + "[19~", "s-f8": ESC + "[19;2~",
    "s-f10": ESC + "[21;2~", "f12": ESC + "[24~", "s-f12": ESC + "[24;2~",
    "s-down": ESC + "[1;2B", "s-end": ESC + "[1;2F", "home": ESC + "[H", "end": ESC + "[F",
    "as-down": ESC + "[1;4B", "as-up": ESC + "[1;4A",
    "c-pgdn": ESC + "[6;5~", "c-pgup": ESC + "[5;5~", "c-space": "\x00", "a-enter": ESC + "\r",
}


def ctrl(c):
    return chr(ord(c.lower()) & 0x1F)


def alt(c):
    return ESC + c


def mouse(kind, x, y, mods=0):
    """SGR mouse report at 1-based column x, row y. kind: press, release, drag,
    wheelup, wheeldown, wheelleft, wheelright."""
    code = {"press": 0, "release": 0, "drag": 32, "wheelup": 64, "wheeldown": 65,
            "wheelleft": 66, "wheelright": 67}[kind] + mods
    return f"{ESC}[<{code};{x};{y}{'m' if kind == 'release' else 'M'}"


# A step: ("cap", text) caption marker, ("k", seq) one key, ("t", text) typed,
# ("w", seconds) wait, ("ptr", x, y) move the drawn pointer, ("m", kind, x, y[, mods])
# a mouse event (also moves the pointer; press/release show a click ring).
DEMOS = {
    "edit": [
        ("w", 1.6),
        ("cap", "skink .  opens the folder with the file tree"),
        ("w", 1.2),
        ("cap", "Alt+P  quick open: type part of a name"),
        ("k", alt("p")), ("w", 0.6), ("t", "main"), ("w", 0.9), ("k", K["enter"]), ("w", 1.0),
        ("cap", "Arrows, Home/End: no modes, just type"),
        ("k", K["down"]), ("k", K["down"]), ("k", K["down"]), ("k", K["end"]), ("w", 0.4),
        ("k", K["enter"]), ("t", "    eprintln!(\"looked up {city}\");"), ("w", 0.9),
        ("cap", "Ctrl+D  select the word, again for the next match"),
        ("k", K["down"]), ("k", K["down"]), ("k", K["down"]), ("k", K["home"]),
        ("k", K["right"] * 7), ("w", 0.3),
        ("k", ctrl("d")), ("w", 0.6), ("k", ctrl("d")), ("w", 0.6), ("k", ctrl("d")), ("w", 0.8),
        ("t", "forecast"), ("w", 1.2),
        ("cap", "Esc  back to one cursor · Ctrl+Z / Ctrl+Y undo and redo"),
        ("k", K["esc"]), ("w", 0.5), ("k", ctrl("z")), ("w", 1.2), ("k", ctrl("y")), ("w", 0.8),
        ("cap", "Ctrl+S  save (gutter marks show the git change)"),
        ("k", ctrl("s")), ("w", 1.8),
    ],
    "find": [
        ("w", 1.4),
        ("cap", "Ctrl+F  find as you type, every match marked"),
        ("k", ctrl("f")), ("w", 0.5), ("t", "rain"), ("w", 1.4),
        ("cap", "Tab  replace with ..."),
        ("k", K["tab"]), ("w", 0.4), ("t", "precip"), ("w", 1.0),
        ("cap", "Enter  replaces every match in one undo step"),
        ("k", K["enter"]), ("w", 1.6),
        ("cap", "Alt+S  search the whole project"),
        ("k", K["esc"]), ("k", alt("s")), ("w", 0.5), ("t", "celsius"), ("w", 0.4), ("k", K["enter"]), ("w", 2.0),
        ("cap", "Enter  jumps to a result"),
        ("k", K["down"]), ("k", K["down"]), ("k", K["enter"]), ("w", 1.8),
    ],
    "terminal": [
        ("w", 1.4),
        ("cap", "Shift+F12  a real terminal in a tab"),
        ("k", K["s-f12"]), ("w", 1.6),
        ("t", "git log --oneline"), ("k", K["enter"]), ("w", 1.2),
        ("t", "ls -a"), ("k", K["enter"]), ("w", 1.4),
        ("cap", "F12  flip back to your file"),
        ("k", K["f12"]), ("w", 1.6),
        ("cap", "F12  and back to the shell, still running"),
        ("k", K["f12"]), ("w", 1.6),
        ("cap", "Alt+T  or keep a shell docked at the bottom"),
        ("k", K["f12"]), ("w", 0.6), ("k", alt("t")), ("w", 1.4), ("t", "echo $SHELL"), ("k", K["enter"]), ("w", 1.8),
    ],
    "panes": [
        ("w", 1.4),
        ("cap", "Ctrl+\\  split side by side"),
        ("k", "\x1c"), ("w", 1.0),
        ("cap", "Alt+P  open another file in the new pane"),
        ("k", alt("p")), ("w", 0.5), ("t", "lib"), ("w", 0.6), ("k", K["enter"]), ("w", 1.2),
        ("cap", "Ctrl+W  move between panes"),
        ("k", ctrl("w")), ("w", 0.9), ("k", ctrl("w")), ("w", 0.9),
        ("cap", "Alt+H  show hidden files · Ctrl+B hide the tree"),
        ("k", alt("h")), ("w", 1.2), ("k", ctrl("b")), ("w", 1.2), ("k", ctrl("b")), ("w", 0.8),
        ("cap", "Alt+Z  zoom: just the text"),
        ("k", alt("z")), ("w", 1.6), ("k", alt("z")), ("w", 1.0),
        ("cap", "Ctrl+E  command line · type help for the manual"),
        ("k", ctrl("e")), ("w", 0.4), ("t", "help tabs"), ("w", 0.6), ("k", K["enter"]), ("w", 2.0),
    ],
    "exit": [
        ("w", 1.2),
        ("cap", "Type something, then Ctrl+Q to quit"),
        ("k", K["end"]), ("k", K["enter"]), ("t", "// todo: hourly view"), ("w", 0.8),
        ("k", ctrl("q")), ("w", 1.4),
        ("cap", "Unsaved? Skink asks first"),
        ("w", 1.4),
        ("cap", "Ctrl+S then Ctrl+Q: saved and out"),
        ("k", K["esc"]), ("w", 0.4), ("k", ctrl("s")), ("w", 0.8), ("k", ctrl("q")), ("w", 1.2),
    ],
}

def cmd(text, wait=1.2):
    """Ctrl-E, type a command, Enter."""
    return [("k", ctrl("e")), ("w", 0.3), ("t", text), ("w", 0.4), ("k", K["enter"]), ("w", wait)]


def goto(line):
    return [("k", ctrl("l")), ("w", 0.2), ("t", str(line)), ("k", K["enter"]), ("w", 0.4)]


DEMOS.update({
    "editor": [
        ("w", 1.2),
        ("cap", "Shift+F12  a terminal; run git commit there"),
        ("k", K["s-f12"]), ("w", 1.4), ("t", "git commit -a"), ("k", K["enter"]), ("w", 1.8),
        ("cap", "The message opens as a tab of this Skink, lean: rulers, # lines dimmed"),
        ("w", 1.2), ("t", "Drop the hourly forecast note"), ("w", 1.2),
        ("cap", "Ctrl+S  save · Ctrl+Q  done, and git carries on"),
        ("k", ctrl("s")), ("w", 0.6), ("k", ctrl("q")), ("w", 2.0),
        ("cap", ":cq  instead would cancel, and git aborts the commit"),
        ("w", 2.0),
    ],
    "rebase": [
        ("w", 1.2),
        ("cap", "git rebase -i  in a Skink terminal"),
        ("k", K["s-f12"]), ("w", 1.4), ("t", "git stash -q; git rebase -i HEAD~1"), ("k", K["enter"]), ("w", 1.8),
        ("cap", "Alt+Shift+K  cycles pick, reword, edit, squash, fixup, drop"),
        ("k", ESC + "K"), ("w", 0.9), ("k", ESC + "K"), ("w", 0.9), ("k", ESC + "K"), ("w", 0.9),
        ("k", ESC + "K"), ("w", 0.9), ("k", ESC + "K"), ("w", 1.2),
        ("cap", ":cq  cancels: the rebase is aborted, nothing changes"),
        *cmd("cq", 2.4),
    ],
    "typed": [
        ("w", 1.4),
        ("cap", "Ctrl+E  then type a command: dotfiles"),
        *cmd("dotfiles", 1.4),
        ("cap", "recent  is quick open, no Alt needed"),
        ("k", ctrl("e")), ("w", 0.3), ("t", "recent"), ("w", 0.4), ("k", K["enter"]), ("w", 0.6),
        ("t", "main"), ("w", 0.6), ("k", K["enter"]), ("w", 1.2),
        ("cap", "comment  toggles the comment on this line"),
        *cmd("comment", 1.4),
        ("cap", "undo  works too, like every action"),
        *cmd("undo", 1.2),
        ("cap", "F1  the palette shows each key and its typed command"),
        ("k", K["f1"]), ("w", 0.5), ("t", "zoom"), ("w", 2.4), ("k", K["esc"]), ("w", 0.8),
    ],
    "config": [
        ("w", 1.2),
        ("cap", "settings  opens settings.json (made with defaults)"),
        *cmd("settings", 1.8),
        ("k", K["down"] * 8), ("w", 1.6),
        ("cap", "bindings  opens bindings.json, Micro's format"),
        *cmd("bindings", 1.8),
        ("cap", "Save, then reload to apply"),
        *cmd("reload", 1.8),
    ],
    "ai": [
        ("w", 1.2),
        ("cap", "agents  lists supported agents, installed or not"),
        *cmd("agents", 2.4),
        ("k", ctrl("q")), ("w", 0.6),
        ("cap", "Ctrl+G  the AI prompt box for the code at the cursor"),
        *goto(10),
        ("k", ctrl("g")), ("w", 0.8), ("t", "add a doc comment"), ("w", 1.0),
        ("cap", "Shift+Tab  mode: rewrite, implement, ask, search"),
        ("k", ESC + "[Z"), ("w", 0.8), ("k", ESC + "[Z"), ("w", 0.8), ("k", ESC + "[Z"), ("w", 1.0),
        ("cap", "@file  and  #rule  add context; Esc closes"),
        ("t", " @lib"), ("w", 1.6), ("k", K["esc"]), ("w", 0.4), ("k", K["esc"]), ("w", 1.0),
    ],
    "mouse": [
        ("w", 1.2),
        ("cap", "Click to put the cursor anywhere"),
        ("ptr", 40, 14), ("ptr", 14, 5), ("m", "press", 14, 5), ("m", "release", 14, 5), ("w", 1.0),
        ("cap", "Drag to select"),
        ("ptr", 22, 6), ("m", "press", 22, 6), ("m", "drag", 26, 6), ("m", "drag", 30, 6), ("m", "drag", 34, 6),
        ("m", "release", 34, 6), ("w", 1.2),
        ("cap", "Alt+click  adds a cursor on each line"),
        ("m", "press", 9, 5), ("m", "release", 9, 5), ("w", 0.4),
        ("m", "press", 9, 6, 8), ("m", "release", 9, 6, 8), ("w", 0.4),
        ("m", "press", 9, 7, 8), ("m", "release", 9, 7, 8), ("w", 0.5),
        ("t", "// "), ("w", 1.2),
        ("cap", "Esc  one cursor again · Ctrl+Z undoes at every cursor"),
        ("k", K["esc"]), ("w", 0.3), *[("k", ctrl("z")) for _ in range(3)], ("w", 1.0),
        ("cap", "Scroll with the wheel or a trackpad"),
        ("ptr", 50, 12), ("m", "wheeldown", 50, 12), ("w", 0.3), ("m", "wheeldown", 50, 12), ("w", 0.3),
        ("m", "wheeldown", 50, 12), ("w", 0.6), ("m", "wheelup", 50, 12), ("w", 0.3), ("m", "wheelup", 50, 12),
        ("w", 0.3), ("m", "wheelup", 50, 12), ("w", 0.8),
        ("cap", "Click a tab to switch"),
        ("k", alt("p")), ("w", 0.4), ("t", "lib"), ("w", 0.5), ("k", K["enter"]), ("w", 1.0),
        ("ptr", 4, 1), ("m", "press", 4, 1), ("w", 0.25), ("m", "release", 4, 1), ("w", 1.6),
    ],
    "zoom": [
        ("w", 1.2),
        ("cap", "Alt+Z  zoom: only the text"),
        ("k", alt("z")), ("w", 1.8), ("k", alt("z")), ("w", 1.0),
        ("cap", "Ctrl+\\  split, then Alt+= to grow the focused pane"),
        ("k", "\x1c"), ("w", 0.8), ("k", alt("=")), ("w", 0.4), ("k", alt("=")), ("w", 0.4), ("k", alt("=")), ("w", 1.0),
        ("cap", "Alt+-  shrinks it again"),
        ("k", alt("-")), ("w", 0.4), ("k", alt("-")), ("w", 0.4), ("k", alt("-")), ("w", 0.4), ("k", alt("-")), ("w", 1.2),
        ("cap", "Bigger text? That's your terminal: Ctrl+= in most"),
        ("w", 2.2),
    ],
    "hidden": [
        ("w", 1.4),
        ("cap", "Alt+H  show hidden files (.env, .gitignore)"),
        ("k", alt("h")), ("w", 1.6), ("k", alt("h")), ("w", 1.0),
        ("cap", "Ctrl+B  hide the file tree · again to bring it back"),
        ("k", ctrl("b")), ("w", 1.4), ("k", ctrl("b")), ("w", 1.0),
        ("cap", "F8  move the keys into the tree"),
        ("k", K["f8"]), ("w", 0.6), ("k", K["down"]), ("k", K["right"]), ("w", 0.4), ("k", K["down"]), ("k", K["down"]),
        ("k", K["enter"]), ("w", 1.2),
        ("cap", "Alt+Shift+B  hide or show the inline blame"),
        ("k", K["down"] * 3), ("w", 0.8), ("k", ESC + "B"), ("w", 1.4), ("k", ESC + "B"), ("w", 1.2),
    ],
    "splits": [
        ("w", 1.2),
        ("cap", "Ctrl+\\  split side by side"),
        ("k", "\x1c"), ("w", 0.9),
        ("cap", "Alt+P  open another file in the new pane"),
        ("k", alt("p")), ("w", 0.4), ("t", "lib"), ("w", 0.5), ("k", K["enter"]), ("w", 1.0),
        ("cap", "Alt+\\  split top and bottom"),
        ("k", ESC + "\\"), ("w", 1.0),
        ("cap", "Ctrl+W  next pane"),
        ("k", ctrl("w")), ("w", 0.7), ("k", ctrl("w")), ("w", 0.7),
        ("cap", "Alt+T  a shell docked along the bottom"),
        ("k", alt("t")), ("w", 1.6), ("t", "ls src"), ("k", K["enter"]), ("w", 1.2),
        ("cap", "F12  back to the file · Alt+T hides the dock (the shell keeps running)"),
        ("k", K["f12"]), ("w", 0.6), ("k", alt("t")), ("w", 1.0),
        ("cap", "Alt+Q  close the focused pane (its tab stays open)"),
        ("k", alt("q")), ("w", 0.8), ("k", alt("q")), ("w", 1.2),
    ],
    "cursors": [
        ("w", 1.2),
        ("cap", "Ctrl+D  select the word, again for the next match"),
        *goto(4), ("k", K["home"]), ("k", K["right"] * 4), ("w", 0.3),
        ("k", ctrl("d")), ("w", 0.6), ("k", ctrl("d")), ("w", 0.6), ("k", ctrl("d")), ("w", 0.8),
        ("cap", "Type once, every cursor edits"),
        ("t", "town"), ("w", 1.2),
        ("cap", "Esc  back to one cursor · Ctrl+Z undoes at every cursor"),
        ("k", K["esc"]), ("w", 0.4), *[("k", ctrl("z")) for _ in range(5)], ("w", 1.0),
        ("cap", "Alt+Shift+↓  add a cursor on the line below"),
        *goto(6), ("k", K["home"]),
        ("k", K["as-down"]), ("w", 0.6), ("t", "e"), ("w", 1.4),
        ("k", K["esc"]), ("k", ctrl("z")), ("w", 1.0),
    ],
    "findreplace": [
        ("w", 1.2),
        ("cap", "Ctrl+F  find as you type; every match is marked"),
        ("k", ctrl("f")), ("w", 0.4), ("t", "celsius"), ("w", 1.2),
        ("cap", "Alt+W  whole word · Alt+C case · Alt+R regex"),
        ("k", alt("w")), ("w", 1.2),
        ("cap", "Tab  then the replacement"),
        ("k", K["tab"]), ("w", 0.4), ("t", "temp_c"), ("w", 1.0),
        ("cap", "Enter  replaces them all, as one undo step"),
        ("k", K["enter"]), ("w", 1.6),
        ("cap", "Ctrl+Z  undo"),
        ("k", ctrl("z")), ("w", 1.4),
    ],
    "projsearch": [
        ("w", 1.2),
        ("cap", "Alt+S  search every file in the project"),
        ("k", alt("s")), ("w", 0.5), ("t", "report"), ("w", 0.4), ("k", K["enter"]), ("w", 1.8),
        ("cap", "Results stream into a tab; Enter jumps"),
        ("k", K["down"] * 3), ("w", 0.6), ("k", K["enter"]), ("w", 1.4),
        ("cap", "sgrep  structural search: code with $A placeholders"),
        ("k", alt("1")), ("w", 0.6), *cmd("sgrep fetch($A)", 2.2),
    ],
    "lsp": [
        ("w", 1.0),
        ("cap", "rust-analyzer starts on its own when you open Rust"),
        ("w", 14.0),
        ("cap", "Alt+K  type and docs at the cursor"),
        *goto(5), ("k", K["home"]), ("k", K["right"] * 13), ("w", 0.4), ("k", alt("k")), ("w", 2.4), ("k", K["esc"]), ("w", 0.4),
        ("cap", "Alt+G  go to the definition · Alt+U back"),
        ("k", alt("g")), ("w", 1.8), ("k", alt("u")), ("w", 1.2),
        ("cap", "Completion opens as you type"),
        *goto(7), ("k", K["end"]), ("k", K["enter"]), ("t", "report."), ("w", 2.6),
        ("t", "ci"), ("w", 1.4), ("k", K["esc"]), ("w", 0.3), ("k", ctrl("z")), ("w", 1.0),
    ],
    "git": [
        ("w", 1.6),
        ("cap", "Gutter marks show changes; Alt+Shift+N jumps to the next"),
        ("k", ESC + "N"), ("w", 1.4),
        ("cap", "Alt+Shift+D  peek: s stage, u unstage, r revert"),
        ("k", ESC + "D"), ("w", 1.8), ("t", "s"), ("w", 1.2),
        ("cap", "Alt+Shift+C  the git panel"),
        ("k", ESC + "C"), ("w", 2.4), ("k", K["esc"]), ("w", 0.6),
        ("cap", "blame  the whole file's history in a tab"),
        ("k", ctrl("q")), ("w", 0.6), *cmd("blame", 2.4),
    ],
    "tasks": [
        ("w", 1.2),
        ("cap", "Shift+F10  pick a task: found in Cargo.toml"),
        ("k", K["s-f10"]), ("w", 1.6), ("t", "test"), ("w", 0.8),
        ("cap", "It runs in a terminal tab"),
        ("k", K["enter"]), ("w", 9.0),
        ("cap", "All green. Failures would show in the gutter, like a language server's"),
        ("w", 2.4),
    ],
    "refactor": [
        ("w", 1.0),
        ("cap", "rust-analyzer starts in the background"),
        ("w", 14.0),
        ("cap", "Alt+Shift+R  rename a symbol used in two files"),
        ("k", ctrl("f")), ("w", 0.3), ("t", "fahrenheit"), ("k", K["enter"]), ("w", 0.5),
        ("k", ESC + "R"), ("w", 1.0), ("k", K["bs"] * 10), ("t", "to_fahrenheit"), ("w", 0.6),
        ("k", K["enter"]), ("w", 3.0),
        ("cap", "The refactor preview: files, hunks, a diff; Space toggles"),
        ("k", K["down"]), ("w", 0.8), ("k", K["down"]), ("w", 1.4),
        ("cap", "Enter  applies the checked changes"),
        ("k", K["enter"]), ("w", 2.0),
    ],
    "folding": [
        ("w", 1.2),
        ("cap", "Alt+M  fold the block at the cursor"),
        *goto(8), ("k", alt("m")), ("w", 1.4),
        ("cap", "Alt+M  again to open it"),
        ("k", alt("m")), ("w", 1.2),
        ("cap", "foldall  closes every top-level block"),
        *cmd("foldall", 1.6),
        ("cap", "Alt+Shift+M  opens every fold"),
        ("k", ESC + "M"), ("w", 1.6),
    ],
    "keys": [
        ("w", 1.2),
        ("cap", "F1  every action with its key and typed command"),
        ("k", K["f1"]), ("w", 0.5), ("t", "comment"), ("w", 2.2), ("k", K["esc"]), ("w", 0.6),
        ("cap", "bindings  opens bindings.json"),
        *cmd("bindings", 1.4),
        ("cap", "Key name and action, Micro's format"),
        ("k", K["end"]), ("k", K["enter"]), ("t", '"Alt-j": "ToggleComment"'), ("w", 1.2),
        ("cap", "Ctrl+S  then reload"),
        ("k", ctrl("s")), ("w", 0.6), *cmd("reload", 1.8),
    ],
})

OPEN = {"edit": ["."], "find": ["src/lib.rs"], "terminal": ["src/main.rs"], "panes": ["src/main.rs"],
        "exit": ["README.md"], "typed": ["."], "config": ["src/main.rs"], "ai": ["src/main.rs"],
        "mouse": ["src/main.rs"], "zoom": ["src/main.rs"], "hidden": ["."], "splits": ["src/main.rs"],
        "cursors": ["src/main.rs"], "findreplace": ["src/lib.rs"], "projsearch": ["src/main.rs"],
        "lsp": ["src/main.rs"], "git": ["src/lib.rs"], "tasks": ["src/main.rs"], "refactor": ["src/main.rs"],
        "folding": ["src/lib.rs"], "keys": ["src/main.rs"], "editor": ["src/lib.rs"], "rebase": ["src/main.rs"]}


def setup(home):
    proj = os.path.join(home, "forecast")
    shutil.copytree(os.path.join(HERE, "demo-project"), proj)
    with open(os.path.join(home, ".gitconfig"), "w") as f:
        f.write("[user]\n\tname = Demo\n\temail = demo@example.com\n[core]\n\teditor = skink\n"
                "[init]\n\tdefaultBranch = main\n[advice]\n\tdetachedHead = false\n")
    with open(os.path.join(home, ".bashrc"), "w") as f:
        f.write("PS1='\\[\\e[38;2;122;162;247m\\]forecast\\[\\e[0m\\] \\$ '\n")
    env = {"GIT_AUTHOR_NAME": "Demo", "GIT_AUTHOR_EMAIL": "demo@example.com",
           "GIT_COMMITTER_NAME": "Demo", "GIT_COMMITTER_EMAIL": "demo@example.com", "HOME": home}
    run = lambda *a: subprocess.run(a, cwd=proj, env={**os.environ, **env}, check=True, capture_output=True)
    run("git", "init", "-q", "-b", "main")
    run("git", "add", "-A")
    run("git", "commit", "-qm", "Forecast: city summary")
    with open(os.path.join(proj, "src", "lib.rs"), "a") as f:
        f.write("\n// TODO: hourly forecast\n")
    run("git", "commit", "-qam", "Note the hourly view")
    shutil.copy(os.path.join(HERE, "demo-project", "src", "lib.rs"), os.path.join(proj, "src", "lib.rs"))
    return proj


def scrub(text, home):
    """Replace the throwaway HOME (or a truncated prefix of it) with ~,
    padded to the same width so the screen layout does not shift."""
    out, i = [], 0
    while True:
        j = text.find(home[:12], i)
        if j < 0:
            out.append(text[i:])
            return "".join(out)
        out.append(text[i:j])
        k = 0
        while k < len(home) and j + k < len(text) and text[j + k] == home[k]:
            k += 1
        if k == len(home):
            end = j + k
            while end < len(text) and text[end] not in " \x1b\"'":
                end += 1
            repl = "~" + text[j + k:end]
        else:
            end = j + k
            repl = "~/forecast"[:k]
        out.append(repl.ljust(end - j)[: end - j])
        i = end


def record(name, skink):
    home = tempfile.mkdtemp(prefix="skink-demo-")
    try:
        proj = setup(home)
        env = {
            "HOME": home, "TERM": "xterm-256color", "COLORTERM": "truecolor", "LANG": "C.UTF-8",
            "PATH": os.path.dirname(skink) + os.pathsep + os.environ["PATH"], "SHELL": "/bin/bash", "USER": "demo",
            "SKINK_NO_UPDATE_CHECK": "1", "SKINK_CONFIG_DIR": os.path.join(home, ".config", "skink"),
            "XDG_STATE_HOME": os.path.join(home, ".state"), "SKINK_ICONS": "nerd",
        }
        argv = [skink, *OPEN[name]]
        bwrap = shutil.which("bwrap")
        if bwrap:
            # Show the throwaway home as /home/demo, so no local path can reach the screen.
            real_home = os.path.expanduser("~")
            argv = [bwrap, "--dev-bind", "/", "/", "--tmpfs", "/home", "--bind", home, "/home/demo"]
            for d in (".cargo", ".rustup", ".local/bin"):
                if os.path.isdir(os.path.join(real_home, d)):
                    argv += ["--bind", os.path.join(real_home, d), os.path.join(real_home, d)]
            argv += ["--chdir", "/home/demo/forecast", "--", skink, *OPEN[name]]
            env.update({"HOME": "/home/demo", "SKINK_CONFIG_DIR": "/home/demo/.config/skink",
                        "XDG_STATE_HOME": "/home/demo/.state",
                        "CARGO_HOME": os.path.join(real_home, ".cargo"),
                        "RUSTUP_HOME": os.path.join(real_home, ".rustup")})
            proj_cwd = None
        else:
            proj_cwd = proj
        pid, fd = pty.fork()
        if pid == 0:
            if proj_cwd:
                os.chdir(proj_cwd)
            os.execve(argv[0], argv, env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", H, W, 0, 0))
        start = time.time()
        events = []

        def pump(secs):
            end = time.time() + secs
            while True:
                left = end - time.time()
                if left <= 0:
                    return
                r, _, _ = select.select([fd], [], [], left)
                if r:
                    try:
                        data = os.read(fd, 65536)
                    except OSError:
                        return
                    if not data:
                        return
                    events.append([round(time.time() - start, 4), "o", data.decode("utf-8", "replace")])

        ptr = []
        for step in DEMOS[name]:
            kind, val = step[0], step[1] if len(step) > 1 else None
            if kind == "ptr":
                ptr.append([round(time.time() - start, 4), step[1], step[2], "move"])
                pump(0.35)
            elif kind == "m":
                _, mk, x, y, *mods = step
                ptr.append([round(time.time() - start, 4), x, y, mk])
                os.write(fd, mouse(mk, x, y, mods[0] if mods else 0).encode())
                pump(0.12)
            elif kind == "w":
                pump(val)
            elif kind == "cap":
                events.append([round(time.time() - start, 4), "m", val])
            elif kind == "k":
                if val:
                    os.write(fd, val.encode())
                    pump(0.12)
            elif kind == "t":
                for ch in val:
                    os.write(fd, ch.encode())
                    pump(0.055)
        try:
            os.kill(pid, 9)
            os.waitpid(pid, 0)
        except OSError:
            pass
        events.sort(key=lambda e: e[0])
        # Squash idle gaps (language servers starting, builds) so the site
        # needs no idleTimeLimit and the pointer track stays in sync.
        GAP = 1.6
        timeline = sorted([(e[0], "e", e) for e in events] + [(p_[0], "p", p_) for p_ in ptr], key=lambda t: t[0])
        shift, last = 0.0, 0.0
        for t, _, item in timeline:
            if t - last > GAP:
                shift += (t - last) - GAP
            last = t
            item[0] = round(t - shift, 4)
        for e in events:
            if e[1] == "o":
                e[2] = scrub(e[2], home)
        os.makedirs(OUT, exist_ok=True)
        path = os.path.join(OUT, f"{name}.cast")
        with open(path, "w") as f:
            f.write(json.dumps({"version": 2, "width": W, "height": H,
                                "title": f"Skink: {name}", "env": {"TERM": "xterm-256color"}}) + "\n")
            for e in events:
                f.write(json.dumps(e, ensure_ascii=False) + "\n")
        ppath = os.path.join(OUT, f"{name}.ptr.json")
        if ptr:
            with open(ppath, "w") as f:
                json.dump({"cols": W, "rows": H, "track": ptr}, f)
        elif os.path.exists(ppath):
            os.remove(ppath)
        user = os.path.basename(os.path.expanduser("~"))
        leaks = [e for e in events if e[1] == "o" and (home in e[2] or user in e[2] or "/tmp/" in e[2])]
        print(f"{name}: {len(events)} events, {events[-1][0]:.1f}s -> {path}"
              + (f"  WARNING: {len(leaks)} events mention local paths" if leaks else ""))
    finally:
        shutil.rmtree(home, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--skink", default=shutil.which("skink"))
    ap.add_argument("--label", help="version label shown on the site (default: skink --version)")
    ap.add_argument("demos", nargs="*", default=list(DEMOS))
    a = ap.parse_args()
    if not a.skink:
        sys.exit("skink not found; pass --skink PATH")
    for d in a.demos:
        record(d, os.path.abspath(a.skink))
    ver = a.label or subprocess.run([a.skink, "--version"], capture_output=True, text=True).stdout.split()[-1]
    with open(os.path.join(OUT, "version.json"), "w") as f:
        json.dump({"version": ver}, f)


if __name__ == "__main__":
    main()
