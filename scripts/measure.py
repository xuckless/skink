#!/usr/bin/env python3
"""Cold start + peak RSS of the release binary drawing one frame in a real
80x24 pty (Linux/macOS). Usage: scripts/measure.py [path] [runs]"""
import fcntl, os, pty, struct, subprocess, sys, termios, time

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
subprocess.run(["cargo", "build", "--release", "-q"], cwd=root, check=True)
binary = os.path.join(root, "target", "release", "skink")
target = sys.argv[1] if len(sys.argv) > 1 else os.path.join(root, "README.md")
runs = int(sys.argv[2]) if len(sys.argv) > 2 else 5
print(f"binary size: {os.path.getsize(binary) / 1024:.0f} KB")

for _ in range(runs):
    t0 = time.perf_counter()
    pid, fd = pty.fork()
    if pid == 0:
        os.execv(binary, [binary, "--exit-after-first-frame", target])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    out = b""
    while True:
        try:
            d = os.read(fd, 65536)
        except OSError:
            break
        if not d:
            break
        out += d
    _, status, ru = os.wait4(pid, 0)
    ms = (time.perf_counter() - t0) * 1000
    rss = ru.ru_maxrss / (1024 if sys.platform == "darwin" else 1)
    print(f"start+first frame {ms:6.1f} ms   peak RSS {rss / 1024:5.1f} MB   exit {status}   output {len(out)} B")
