//! Which TCP ports the programs in the terminal tabs listen on (the remote
//! side of `skink ssh`): the tab's shell and everything it started.
//! Linux reads `/proc` (no processes spawned); macOS asks `ps` and `lsof`.
//! Other systems report nothing.

use std::collections::BTreeSet;

/// Listening TCP ports owned by `roots` or their descendants.
pub fn listening(roots: &[u32]) -> BTreeSet<u16> {
    if roots.is_empty() {
        return BTreeSet::new();
    }
    imp::listening(roots)
}

/// All descendants of `roots` (and the roots), from `(pid, ppid)` pairs.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn tree(roots: &[u32], pairs: &[(u32, u32)]) -> BTreeSet<u32> {
    let mut all: BTreeSet<u32> = roots.iter().copied().collect();
    loop {
        let before = all.len();
        for &(pid, ppid) in pairs {
            if all.contains(&ppid) {
                all.insert(pid);
            }
        }
        if all.len() == before {
            return all;
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::{BTreeSet, HashSet};

    pub fn listening(roots: &[u32]) -> BTreeSet<u16> {
        let pairs: Vec<(u32, u32)> = std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let pid: u32 = e.file_name().to_str()?.parse().ok()?;
                Some((pid, ppid(pid)?))
            })
            .collect();
        let pids = super::tree(roots, &pairs);
        let inodes: HashSet<u64> = pids.iter().flat_map(|&p| socket_inodes(p)).collect();
        let mut out = BTreeSet::new();
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            let Ok(text) = std::fs::read_to_string(table) else {
                continue;
            };
            out.extend(listen_ports(&text, &inodes));
        }
        out
    }

    fn ppid(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // "pid (comm) state ppid ...": comm may hold spaces and parens.
        stat.rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    }

    fn socket_inodes(pid: u32) -> Vec<u64> {
        std::fs::read_dir(format!("/proc/{pid}/fd"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let link = std::fs::read_link(e.path()).ok()?;
                let s = link.to_str()?;
                s.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()
            })
            .collect()
    }

    /// Ports in LISTEN state (`st` 0A) whose socket inode is one of ours.
    pub fn listen_ports(table: &str, inodes: &HashSet<u64>) -> Vec<u16> {
        table
            .lines()
            .skip(1)
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                if f.get(3) != Some(&"0A") {
                    return None;
                }
                let inode: u64 = f.get(9)?.parse().ok()?;
                if !inodes.contains(&inode) {
                    return None;
                }
                let port = f.get(1)?.rsplit_once(':')?.1;
                u16::from_str_radix(port, 16).ok()
            })
            .collect()
    }

    #[cfg(test)]
    #[test]
    fn the_proc_table_is_read_by_state_and_inode() {
        let t = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1\n   1: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 999 1\n   2: 0100007F:0BB9 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 4242 1\n";
        let ours: HashSet<u64> = [4242].into();
        assert_eq!(listen_ports(t, &ours), [3000]);
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::BTreeSet;
    use std::process::{Command, Stdio};

    pub fn listening(roots: &[u32]) -> BTreeSet<u16> {
        let run = |c: &mut Command| {
            c.stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        };
        let ps = run(Command::new("ps").args(["-A", "-o", "pid=,ppid="]));
        let pairs: Vec<(u32, u32)> = ps
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            })
            .collect();
        let pids: Vec<String> = super::tree(roots, &pairs)
            .iter()
            .map(u32::to_string)
            .collect();
        let lsof = run(Command::new("lsof").args([
            "-nP",
            "-a",
            "-iTCP",
            "-sTCP:LISTEN",
            "-Fn",
            "-p",
            &pids.join(","),
        ]));
        lsof.lines()
            .filter_map(|l| l.strip_prefix('n')?.rsplit_once(':')?.1.parse().ok())
            .collect()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use std::collections::BTreeSet;

    pub fn listening(_: &[u32]) -> BTreeSet<u16> {
        BTreeSet::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_are_followed() {
        let pairs = [(2, 1), (3, 2), (4, 9), (5, 3)];
        assert_eq!(tree(&[1], &pairs), [1, 2, 3, 5].into());
    }

    /// A real listener in a child of this process is found (Linux /proc,
    /// macOS lsof); a port nobody in the tree owns is not.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_child_listening_on_a_port_is_seen() {
        use std::io::BufRead;
        // This process listening, with itself as the root.
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let got = listening(&[std::process::id()]);
        assert!(got.contains(&port), "{port} not in {got:?}");
        // A process outside the tree (a sleeping child) does not own it.
        let mut sleeper = std::process::Command::new("sleep")
            .arg("20")
            .spawn()
            .unwrap();
        let other = listening(&[sleeper.id()]);
        let _ = sleeper.kill();
        let _ = sleeper.wait();
        assert!(!other.contains(&port), "{other:?}");
        // A grandchild (sh -> python3) that starts listening by itself;
        // skipped without python3.
        let has_py = std::process::Command::new("python3")
            .arg("-c")
            .arg("0")
            .output()
            .is_ok_and(|o| o.status.success());
        if !has_py {
            return;
        }
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "exec python3 -c 'import socket,sys,time\ns=socket.socket(); s.bind((\"127.0.0.1\",0)); s.listen(); print(s.getsockname()[1], flush=True); time.sleep(20)'",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
        let _ = out.read_line(&mut line);
        let child_port: u16 = line.trim().parse().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = false;
        while std::time::Instant::now() < deadline {
            if listening(&[std::process::id()]).contains(&child_port) {
                seen = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(seen, "the child's port {child_port} was not seen");
    }
}
