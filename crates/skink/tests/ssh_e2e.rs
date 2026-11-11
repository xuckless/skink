//! `skink ssh` against a real, unprivileged OpenSSH server on loopback:
//! ControlMaster, probe, install over the connection, `-O forward`.
//! Ignored by default (needs `sshd` and `ssh-keygen`; ~2 s):
//! `cargo test -p skink --test ssh_e2e -- --ignored`.
#![cfg(unix)]

use skink_ui::ssh::{self, Job, Mux, Origin, Ssh};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run(c: &mut Command) {
    let s = c.stdin(Stdio::null()).status().expect("run");
    assert!(s.success(), "{c:?}");
}

#[test]
#[ignore = "starts a real sshd on loopback"]
fn install_and_forward_over_a_real_sshd() {
    let sshd = ["/usr/sbin/sshd", "/usr/bin/sshd"]
        .into_iter()
        .find(|p| Path::new(p).exists());
    let Some(sshd) = sshd else {
        eprintln!("no sshd: skipped");
        return;
    };
    let d = std::env::temp_dir().join(format!("skssh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("home")).unwrap();
    for k in ["hostkey", "id"] {
        run(Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(d.join(k)));
    }
    std::fs::copy(d.join("id.pub"), d.join("authorized_keys")).unwrap();
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let p = |n: &str| d.join(n).display().to_string();
    std::fs::write(
        d.join("sshd_config"),
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\nPidFile {}\nSetEnv HOME={}\nAllowTcpForwarding yes\n",
            p("hostkey"),
            p("authorized_keys"),
            p("sshd.pid"),
            p("home")
        ),
    )
    .unwrap();
    let mut server = Command::new(sshd)
        .args(["-D", "-e", "-f"])
        .arg(d.join("sshd_config"))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "sshd did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "root".into());
    let s = Ssh {
        program: "ssh".into(),
        dest: format!("{user}@127.0.0.1"),
        port: Some(port),
        mux: Mux::Control(d.join("m.sock")),
        options: [
            "-F",
            "/dev/null",
            "-i",
            &p("id"),
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            &format!("UserKnownHostsFile={}", p("known_hosts")),
        ]
        .map(String::from)
        .to_vec(),
    };
    let result = std::panic::catch_unwind(|| {
        s.connect().expect("master");
        let before = ssh::probe(&s).expect("probe");
        assert!(
            before.installed == skink_ui::ssh::Remote::Missing,
            "{before:?}"
        );
        ssh::upload(&s, Path::new(env!("CARGO_BIN_EXE_skink"))).expect("upload");
        let after = ssh::probe(&s).expect("probe");
        assert!(!after.needs_install(), "{after:?}");
        assert!(d.join("home/.skink/bin/skink").is_file());

        // A "remote" server (same machine): its port is taken here, so the
        // forward lands on the next free one.
        let srv = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let rport = srv.local_addr().unwrap().port();
        let mut f = ssh::Forwarder::new(s.clone(), "tok".into(), skink_config::AutoForward::On);
        f.handle(Job::Forward {
            remote: rport,
            local: 0,
            origin: Origin::Manual,
        });
        let (_, local) = f.active()[0];
        assert_ne!(local, rport);
        let t = std::thread::spawn(move || {
            let (mut c, _) = srv.accept().unwrap();
            c.write_all(b"through the tunnel").unwrap();
        });
        let mut c = std::net::TcpStream::connect(("127.0.0.1", local)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut got = String::new();
        let _ = c.read_to_string(&mut got);
        assert_eq!(got, "through the tunnel");
        t.join().unwrap();
        f.handle(Job::Stop);
        assert!(std::net::TcpStream::connect(("127.0.0.1", local)).is_err());
    });
    let _ = Command::new("ssh")
        .args(["-S", &p("m.sock"), "-O", "exit", "x"])
        .stderr(Stdio::null())
        .status();
    let _ = server.kill();
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&d);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
