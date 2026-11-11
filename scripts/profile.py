#!/usr/bin/env python3
"""Profile the real skink binary in a pty (Linux; /proc is needed).

Runs a scripted session (open the repo, several source files, a terminal tab,
a ~300 KB and a ~1 MB Rust file, a paste), samples the editor's memory after
each step, measures idle CPU and the time to the first frame, and with
--budget fails when a number exceeds scripts/budgets.json.

    scripts/profile.py                 build release, print the table
    scripts/profile.py --budget        ... and exit 1 on a budget overrun
    scripts/profile.py --binary PATH   profile an existing binary
    scripts/profile.py --huge 2        also open a 2 GB log in the large-file
                                       view (not a CI budget: it writes the
                                       file, default under target/)

Columns: RSS (resident), anon (heap etc.), USS (private to the process),
children (shells, language servers; reported, not budgeted).
"""
import argparse, fcntl, json, os, pty, struct, subprocess, sys, tempfile, termios, threading, time

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
HZ = os.sysconf("SC_CLK_TCK")


def status(pid):
    d = {}
    try:
        for line in open(f"/proc/{pid}/status"):
            k, _, v = line.partition(":")
            if k in ("VmRSS", "VmHWM", "RssAnon", "Threads", "Name"):
                d[k] = v.strip()
        for line in open(f"/proc/{pid}/smaps_rollup"):
            k, _, v = line.partition(":")
            if k in ("Private_Clean", "Private_Dirty"):
                d[k] = int(v.split()[0])
    except OSError:
        return None
    return d


def mb(v):
    return (int(v.split()[0]) if isinstance(v, str) else v) / 1024


def children(pid):
    out = []
    try:
        for t in os.listdir(f"/proc/{pid}/task"):
            for c in open(f"/proc/{pid}/task/{t}/children").read().split():
                out += [int(c)] + children(int(c))
    except OSError:
        pass
    return out


def cpu_seconds(pid):
    f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) / HZ


def first_frame_ms(binary, target, runs=7):
    """Median time from exec to the first frame being written (80x24 pty)."""
    times = []
    for _ in range(runs):
        t0 = time.perf_counter()
        pid, fd = pty.fork()
        if pid == 0:
            os.execv(binary, [binary, "--exit-after-first-frame", target])
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        while True:
            try:
                if not os.read(fd, 65536):
                    break
            except OSError:
                break
        os.waitpid(pid, 0)
        times.append((time.perf_counter() - t0) * 1000)
    return sorted(times)[len(times) // 2]


def make_inputs(tmp):
    """A ~300 KB and a ~1 MB Rust file built from the repo's own sources."""
    src = []
    for d, _, files in os.walk(os.path.join(ROOT, "crates")):
        src += [os.path.join(d, f) for f in sorted(files) if f.endswith(".rs")]
    text = "".join(open(p, encoding="utf-8").read() for p in sorted(src))
    paths = {}
    for name, size in (("mid.rs", 300_000), ("big.rs", 1_000_000)):
        body = (text * (size // max(len(text), 1) + 1))[:size]
        body = body[: body.rfind("\n") + 1]
        paths[name] = os.path.join(tmp, name)
        with open(paths[name], "w", encoding="utf-8") as f:
            f.write(body)
    return paths


def session(binary, tmp, inputs):
    env = dict(
        os.environ,
        PATH="/usr/local/bin:/usr/bin:/bin",  # no language servers: measure the editor
        SKINK_ICONS="ascii",
        SKINK_CONFIG_DIR=os.path.join(tmp, "cfg"),
        SKINK_STATE_DIR=os.path.join(tmp, "state"),
        SKINK_NO_UPDATE_CHECK="1",
        TERM="xterm-256color",
    )
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(ROOT)
        os.execve(binary, [binary, "."], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 200, 0, 0))
    stop = threading.Event()

    def drain():
        while not stop.is_set():
            try:
                if not os.read(fd, 1 << 16):
                    return
            except OSError:
                return

    threading.Thread(target=drain, daemon=True).start()

    def send(s):
        os.write(fd, s.encode())
        time.sleep(0.15)

    def cmd(c):
        send("\x05")  # Ctrl-E: command line
        send(c)
        send("\r")

    rows = []

    def sample(label, settle=1.0):
        time.sleep(settle)
        best = None
        for _ in range(5):
            s = status(pid)
            if s and (best is None or mb(s["VmRSS"]) > mb(best["VmRSS"])):
                best = s
            time.sleep(0.1)
        kids = [k for k in (status(c) for c in children(pid)) if k]
        row = {
            "step": label,
            "rss": mb(best["VmRSS"]),
            "anon": mb(best["RssAnon"]),
            "uss": (best["Private_Clean"] + best["Private_Dirty"]) / 1024,
            "peak": mb(best["VmHWM"]),
            "children": sum(mb(k["VmRSS"]) for k in kids),
        }
        rows.append(row)
        kid = f"   children {row['children']:6.1f}" if kids else ""
        print(
            f"{label:<40} RSS {row['rss']:6.1f} MB  anon {row['anon']:6.1f}  "
            f"USS {row['uss']:6.1f}  peak {row['peak']:6.1f}{kid}",
            flush=True,
        )

    try:
        sample("start: skink . (tree)")
        for f in ["README.md", "crates/skink-ui/src/lib.rs", "crates/skink-ui/src/view.rs",
                  "crates/skink-core/src/editor.rs", "crates/skink-ui/src/tests.rs"]:
            cmd("open " + f)
        sample("+ 5 project files")
        send("\x1b[24;2~")  # Shift-F12: terminal tab
        sample("+ terminal tab", 1.5)
        send("\x1b[6;5~")  # Ctrl-PageDown: back to a document
        cmd("open " + inputs["mid.rs"])
        sample("+ 300 KB Rust file", 2)
        cmd("open " + inputs["big.rs"])
        sample("+ 1 MB Rust file", 3)
        send("\x1b[200~" + "let x = 1; // pasted\n" * 4000 + "\x1b[201~")
        sample("+ 80 KB paste", 2)
        c0 = cpu_seconds(pid)
        time.sleep(5)
        idle_cpu_ms = (cpu_seconds(pid) - c0) * 1000
        print(f"idle CPU over 5 s: {idle_cpu_ms:.0f} ms")
    finally:
        stop.set()
        for c in children(pid):
            try:
                os.kill(c, 9)
            except OSError:
                pass
        os.kill(pid, 9)
        os.waitpid(pid, 0)
    return rows, idle_cpu_ms


def make_log(path, gb):
    """A log of `gb` GB, written once and reused."""
    want = int(gb * (1 << 30))
    if os.path.exists(path) and os.path.getsize(path) >= want:
        return
    line = "2026-11-11T03:04:05.678Z INFO  worker-%05d request id=%08x took %4d ms path=/api/v1/items\n"
    with open(path, "w") as f:
        n = 0
        while n < want:
            chunk = "".join(line % (i % 99999, i * 2654435761 % (1 << 32), i % 9999)
                            for i in range(n // 90, n // 90 + 100000))
            f.write(chunk)
            n += len(chunk)


def huge_session(binary, log):
    """Open `log` (large-file view), go to its end, search; RSS after each."""
    env = dict(os.environ, SKINK_ICONS="ascii", SKINK_NO_UPDATE_CHECK="1", TERM="xterm-256color")
    with tempfile.TemporaryDirectory(prefix="skink-huge-") as tmp:
        env["SKINK_CONFIG_DIR"] = os.path.join(tmp, "cfg")
        env["SKINK_STATE_DIR"] = os.path.join(tmp, "state")
        t0 = time.perf_counter()
        pid, fd = pty.fork()
        if pid == 0:
            os.execve(binary, [binary, log], env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 200, 0, 0))
        out = bytearray()
        stop = threading.Event()

        def drain():
            while not stop.is_set():
                try:
                    b = os.read(fd, 1 << 16)
                except OSError:
                    return
                if not b:
                    return
                out.extend(b)

        threading.Thread(target=drain, daemon=True).start()
        while not out and time.perf_counter() - t0 < 10:
            time.sleep(0.005)
        print(f"  first frame after {(time.perf_counter() - t0) * 1000:.0f} ms", flush=True)

        def rss(label):
            s = status(pid)
            print(f"  {label:<34} RSS {mb(s['VmRSS']):6.1f} MB  peak {mb(s['VmHWM']):6.1f} MB", flush=True)
            return mb(s["VmHWM"])

        def send(s, wait=0.5):
            os.write(fd, s.encode())
            time.sleep(wait)

        try:
            rss("opened")
            # Indexing done when the editor stops using CPU.
            t1 = time.perf_counter()
            while True:
                c0 = cpu_seconds(pid)
                time.sleep(0.5)
                if cpu_seconds(pid) - c0 < 0.05 or time.perf_counter() - t1 > 120:
                    break
            print(f"  indexed in about {time.perf_counter() - t1:.1f} s", flush=True)
            rss("indexed")
            send("\x1b[1;5F", 1)  # Ctrl-End: the last line
            rss("at the end")
            send("\x1b[6~" * 50, 1)  # PageDown x50
            send("\x05" + "1000000" + "\r", 1)
            rss("goto line 1,000,000")
            send("\x06" + "worker-12345" + "\r", 2)  # find
            peak = rss("find")
        finally:
            stop.set()
            os.kill(pid, 9)
            os.waitpid(pid, 0)
    return peak


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--budget", action="store_true", help="fail when scripts/budgets.json is exceeded")
    ap.add_argument("--binary", help="profile this binary instead of building release")
    ap.add_argument("--huge", type=float, metavar="GB", help="also open a log this big (large-file view)")
    ap.add_argument("--huge-file", help="where to write that log (default target/huge.log)")
    a = ap.parse_args()
    if not os.path.isdir("/proc/self"):
        sys.exit("profile.py needs Linux (/proc)")
    binary = a.binary
    if not binary:
        subprocess.run(["cargo", "build", "--release", "-q", "-p", "skink"], cwd=ROOT, check=True)
        binary = os.path.join(ROOT, "target", "release", "skink")
    size_mb = os.path.getsize(binary) / 1e6
    ff = first_frame_ms(binary, os.path.join(ROOT, "README.md"))
    print(f"binary {size_mb:.2f} MB   first frame (median of 7) {ff:.1f} ms")
    if a.huge:
        log = a.huge_file or os.path.join(ROOT, "target", "huge.log")
        os.makedirs(os.path.dirname(log), exist_ok=True)
        make_log(log, a.huge)
        print(f"large-file view: {os.path.getsize(log) / (1 << 30):.2f} GB log", flush=True)
        huge_session(binary, log)
    with tempfile.TemporaryDirectory(prefix="skink-profile-") as tmp:
        rows, idle_cpu = session(binary, tmp, make_inputs(tmp))
    if not a.budget:
        return
    budgets = json.load(open(os.path.join(ROOT, "scripts", "budgets.json")))
    by_step = {r["step"]: r for r in rows}
    checks = [
        ("binary MB", size_mb, budgets["binary_mb"]),
        ("first frame ms", ff, budgets["first_frame_ms"]),
        ("idle RSS MB", by_step["start: skink . (tree)"]["rss"], budgets["idle_rss_mb"]),
        ("RSS with 5 project files MB", by_step["+ 5 project files"]["rss"], budgets["project_rss_mb"]),
        ("RSS with the 1 MB file MB", by_step["+ 1 MB Rust file"]["rss"], budgets["big_file_rss_mb"]),
        ("idle CPU ms / 5 s", idle_cpu, budgets["idle_cpu_ms"]),
    ]
    failed = [c for c in checks if c[1] > c[2]]
    for name, got, limit in checks:
        mark = "FAIL" if got > limit else "ok"
        print(f"  {mark:>4}  {name:<30} {got:8.1f}  (budget {limit})")
    if failed:
        sys.exit(f"{len(failed)} budget(s) exceeded")


if __name__ == "__main__":
    main()
