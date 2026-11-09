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
    "enter": "\r", "tab": "\t", "esc": ESC, "bs": "\x7f",
    "up": ESC + "[A", "down": ESC + "[B", "right": ESC + "[C", "left": ESC + "[D",
    "f5": ESC + "[15~", "f8": ESC + "[19~", "f12": ESC + "[24~", "s-f12": ESC + "[24;2~",
    "s-down": ESC + "[1;2B", "s-end": ESC + "[1;2F", "home": ESC + "[H", "end": ESC + "[F",
    "c-pgdn": ESC + "[6;5~", "c-pgup": ESC + "[5;5~",
}


def ctrl(c):
    return chr(ord(c.lower()) & 0x1F)


def alt(c):
    return ESC + c


# A step: ("cap", text) caption marker, ("k", seq) one key, ("t", text) typed,
# ("w", seconds) wait.
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
        ("cap", "Esc  back to one cursor · Ctrl+Z undoes it all"),
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

OPEN = {"edit": ["."], "find": ["src/lib.rs"], "terminal": ["src/main.rs"], "panes": ["src/main.rs"], "exit": ["README.md"]}


def setup(home):
    proj = os.path.join(home, "forecast")
    shutil.copytree(os.path.join(HERE, "demo-project"), proj)
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
            "PATH": os.environ["PATH"], "SHELL": "/bin/bash", "USER": "demo",
            "SKINK_NO_UPDATE_CHECK": "1", "SKINK_CONFIG_DIR": os.path.join(home, ".config", "skink"),
            "XDG_STATE_HOME": os.path.join(home, ".state"), "SKINK_ICONS": "nerd",
        }
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(proj)
            os.execve(skink, [skink, *OPEN[name]], env)
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

        for kind, val in DEMOS[name]:
            if kind == "w":
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
        leaks = [e for e in events if e[1] == "o" and (home in e[2] or "xuckl" in e[2])]
        print(f"{name}: {len(events)} events, {events[-1][0]:.1f}s -> {path}"
              + (f"  WARNING: {len(leaks)} events mention local paths" if leaks else ""))
    finally:
        shutil.rmtree(home, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--skink", default=shutil.which("skink"))
    ap.add_argument("demos", nargs="*", default=list(DEMOS))
    a = ap.parse_args()
    if not a.skink:
        sys.exit("skink not found; pass --skink PATH")
    for d in a.demos:
        record(d, os.path.abspath(a.skink))
    ver = subprocess.run([a.skink, "--version"], capture_output=True, text=True).stdout.split()[-1]
    with open(os.path.join(OUT, "version.json"), "w") as f:
        json.dump({"version": ver}, f)


if __name__ == "__main__":
    main()
