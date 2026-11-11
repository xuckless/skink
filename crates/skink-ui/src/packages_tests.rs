use super::*;
use std::process::Command;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skink-pkg-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn store(root: &Path) -> Store {
    Store {
        data: root.join("data"),
        fonts: Some(root.join("fonts")),
        index_url: String::new(),
        github: String::new(),
        api: None,
        target: "x86_64-linux".into(),
    }
}

/// The test grammar from skink-syntax (an original grammar, see its README).
fn wasm() -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../skink-syntax/tests/fixtures/skinktest/skinktest.wasm");
    std::fs::read(p).unwrap()
}

fn write(p: &Path, body: impl AsRef<[u8]>) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn package(dir: &Path, version: &str, extra: &str) {
    write(&dir.join("grammars/skinktest.wasm"), wasm());
    write(&dir.join("queries/highlights.scm"), "\"let\" @keyword\n");
    write(&dir.join("themes/pkgtheme.json"), "{}");
    write(&dir.join("init.lua"), "-- hi\n");
    write(
        &dir.join(MANIFEST),
        format!(
            r#"[package]
name = "skinktest"
version = "{version}"
description = "test"
license = "MIT"

[[grammar]]
name = "skinktest"
wasm = "grammars/skinktest.wasm"
highlights = "queries/highlights.scm"
extensions = ["skt"]

[[lsp]]
language = "skinktest"
command = ["sktls", "--stdio"]
extensions = ["skt"]

[[theme]]
path = "themes/pkgtheme.json"

[[script]]
path = "init.lua"
{extra}"#
        ),
    );
}

fn tar_gz(dir: &Path, out: &Path) {
    let ok = Command::new("tar")
        .arg("-czf")
        .arg(out)
        .arg("-C")
        .arg(dir)
        .arg(".")
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

fn sha(p: &Path) -> String {
    update::sha256_file(p).unwrap()
}

/// A loopback HTTP server: `routes` maps a path to (status, body).
fn fake_http(routes: Vec<(String, u16, Vec<u8>)>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in l.incoming().flatten() {
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).map_or(true, |n| n == 0) || h == "\r\n" {
                    break;
                }
            }
            let (code, body) = routes
                .iter()
                .find(|(p, ..)| *p == path)
                .map_or((404, b"nope".to_vec()), |(_, c, b)| (*c, b.clone()));
            let mut s = stream;
            let _ = write!(
                s,
                "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(&body);
        }
    });
    format!("http://{addr}")
}

fn done(job: &Job) -> &Done {
    match job {
        Job::Done(d) => d,
        other => panic!("expected done, got {other}"),
    }
}

#[test]
fn a_local_package_installs_activates_disables_and_uninstalls_cleanly() {
    let root = temp("local");
    let src = root.join("src");
    package(&src, "0.1.0", "");
    let s = store(&root);
    let mut steps = Vec::new();
    let job = s.install(&Spec::Dir(src.clone()), When::Always, &mut |j| {
        steps.push(j.clone())
    });
    assert_eq!(done(&job).change, Change::New);
    assert_eq!(
        steps[..3],
        [Job::Resolving, Job::Verifying, Job::Installing],
        "a folder needs no download"
    );
    let a = s.active();
    assert_eq!(a.grammars.len(), 1);
    assert_eq!(a.grammars[0].extensions, ["skt"]);
    assert_eq!(a.grammars[0].highlights, "\"let\" @keyword\n");
    assert!(a.grammars[0].wasm.is_file());
    assert_eq!(a.lsp[0].0, "skinktest");
    assert_eq!(a.theme_dirs, [s.pkg_dir("skinktest").join("themes")]);
    assert_eq!(a.scripts, [s.pkg_dir("skinktest").join("init.lua")]);

    // Same version again: --update-packages leaves it alone; a new one replaces it.
    let again = s.install(&Spec::Dir(src.clone()), When::IfChanged, &mut |_| {});
    assert_eq!(done(&again).change, Change::Unchanged);
    package(&src, "0.2.0", "");
    let up = s.install(&Spec::Dir(src.clone()), When::IfChanged, &mut |_| {});
    assert_eq!(
        done(&up).change,
        Change::Replaced {
            from: "0.1.0".into()
        }
    );

    s.set_enabled("skinktest", false).unwrap();
    assert_eq!(s.installed()[0].state, PkgState::Disabled);
    assert!(s.active().grammars.is_empty());
    s.set_enabled("skinktest", true).unwrap();

    // A package whose files went missing shows as broken, not as a crash.
    std::fs::remove_file(s.pkg_dir("skinktest").join("queries/highlights.scm")).unwrap();
    assert!(matches!(s.installed()[0].state, PkgState::Broken(_)));
    assert!(s.active().grammars.is_empty());

    s.uninstall("skinktest").unwrap();
    assert!(s.installed().is_empty());
    assert!(!s.pkg_dir("skinktest").exists());
    assert!(s.uninstall("skinktest").is_err());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_github_release_installs_with_its_pinned_tool_and_fonts() {
    if !update::have("curl") {
        return;
    }
    let root = temp("gh");
    // The tool and the font: archives the manifest pins by SHA-256.
    let tool_src = root.join("tool-src");
    // A real Windows download carries the `.exe`; the manifest says `bin/sktls`.
    let tool_file = if cfg!(windows) {
        "bin/sktls.exe"
    } else {
        "bin/sktls"
    };
    write(&tool_src.join(tool_file), "#!/bin/sh\necho hi\n");
    let tool = root.join("sktls.tar.gz");
    tar_gz(&tool_src, &tool);
    let font_src = root.join("font-src");
    write(&font_src.join("Test-Regular.ttf"), "TTF");
    write(&font_src.join("README.md"), "not a font");
    let font = root.join("testfont.tar.gz");
    tar_gz(&font_src, &font);

    // The package names the download URLs, so the files are served first.
    let base_files = fake_http(vec![
        (
            "/files/sktls.tar.gz".into(),
            200,
            std::fs::read(&tool).unwrap(),
        ),
        (
            "/files/testfont.tar.gz".into(),
            200,
            std::fs::read(&font).unwrap(),
        ),
    ]);
    let pkg = root.join("pkg");
    package(
        &pkg,
        "1.0.0",
        &format!(
            r#"
[[tool]]
name = "sktls"
bin = "bin/sktls"
[tool.targets]
"x86_64-linux" = {{ url = "{base_files}/files/sktls.tar.gz", sha256 = "{}" }}
"aarch64-plan9" = {{ url = "{base_files}/files/none.tar.gz", sha256 = "{}" }}

[[font]]
name = "Test Font"
url = "{base_files}/files/testfont.tar.gz"
sha256 = "{}"
files = ["*.ttf"]
"#,
            sha(&tool),
            "0".repeat(64),
            sha(&font)
        ),
    );
    // Like skink-package-action: one folder with the manifest at its top.
    let archive = root.join("skinktest.skink.tar.gz");
    let ok = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&root)
        .arg("pkg")
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let good = std::fs::read(&archive).unwrap();
    let sum = format!("{}  skinktest.skink.tar.gz\n", sha(&archive));
    let gh = fake_http(vec![
        (
            "/me/pkgs/releases/latest/download/skinktest.skink.tar.gz".into(),
            200,
            good.clone(),
        ),
        (
            "/me/pkgs/releases/latest/download/skinktest.skink.tar.gz.sha256".into(),
            200,
            sum.clone().into_bytes(),
        ),
        (
            "/me/pkgs/releases/download/v2/skinktest.skink.tar.gz".into(),
            200,
            good,
        ),
        (
            "/me/pkgs/releases/download/v2/skinktest.skink.tar.gz.sha256".into(),
            200,
            format!("{}  x\n", "f".repeat(64)).into_bytes(),
        ),
    ]);
    let mut s = store(&root);
    s.github = gh;
    let spec = Spec::parse("me/pkgs/skinktest").unwrap();
    let mut steps = Vec::new();
    let job = s.install(&spec, When::Always, &mut |j| steps.push(j.clone()));
    let d = done(&job);
    assert_eq!(
        (d.name.as_str(), d.version.as_str()),
        ("skinktest", "1.0.0")
    );
    // Windows only copies fonts (registering needs the user), and says so.
    let notes: Vec<_> = d
        .notes
        .iter()
        .filter(|n| !(cfg!(windows) && n.starts_with("fonts copied")))
        .collect();
    assert!(notes.is_empty(), "{:?}", d.notes);
    assert_eq!(
        steps[..4],
        [
            Job::Resolving,
            Job::Downloading("skinktest.skink.tar.gz".into()),
            Job::Verifying,
            Job::Installing
        ]
    );
    let inst = &s.installed()[0];
    assert_eq!(inst.source, "me/pkgs/skinktest");
    assert_eq!(inst.placed, [root.join("fonts/Test-Regular.ttf")]);
    assert!(root.join("fonts/Test-Regular.ttf").is_file());
    assert!(!root.join("fonts/README.md").exists());
    let a = s.active();
    // The server the package downloads runs from its tool folder, and the
    // folder is on the server's PATH.
    let skink_lsp::Override::Server { argv, env, .. } = &a.lsp[0].1 else {
        panic!("expected a server");
    };
    assert_eq!(
        Path::new(&argv[0]),
        s.data.join("tools/skinktest/sktls").join(tool_file)
    );
    // Compare as paths, not strings (Windows mixes `\` and `/`).
    assert_eq!(
        std::env::split_paths(&env[0].1).next(),
        Some(
            s.data
                .join("tools")
                .join("skinktest")
                .join("sktls")
                .join("bin")
        )
    );

    // A bad checksum fails at Verifying and leaves the installed one alone.
    let bad = s.install(
        &Spec::parse("me/pkgs/skinktest@v2").unwrap(),
        When::Always,
        &mut |_| {},
    );
    assert!(
        matches!(&bad, Job::Failed { step: Step::Verifying, reason } if reason.contains("checksum")),
        "{bad}"
    );
    assert_eq!(s.installed()[0].version, "1.0.0");

    s.uninstall("skinktest").unwrap();
    assert!(
        !root.join("fonts/Test-Regular.ttf").exists(),
        "fonts go too"
    );
    assert!(!s.data.join("tools/skinktest").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_package_cannot_reach_outside_its_folder() {
    let root = temp("escape");
    let src = root.join("src");
    package(&src, "0.1.0", "");
    let m = std::fs::read_to_string(src.join(MANIFEST)).unwrap();
    write(
        &src.join(MANIFEST),
        m.replace("themes/pkgtheme.json", "../../etc/passwd"),
    );
    let job = store(&root).install(&Spec::Dir(src), When::Always, &mut |_| {});
    assert!(
        matches!(&job, Job::Failed { step: Step::Verifying, reason } if reason.contains("inside the package")),
        "{job}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn verified_names_are_unique_and_look_alikes_are_refused() {
    let v = verified();
    assert!(v.len() >= 20, "verified.json parsed: {}", v.len());
    for (i, a) in v.iter().enumerate() {
        assert!(valid_name(&a.name), "{}", a.name);
        for b in &v[i + 1..] {
            assert_ne!(
                fold(&a.name),
                fold(&b.name),
                "{} and {} look alike",
                a.name,
                b.name
            );
        }
    }
    let s = store(&temp("lookalike"));
    for fake in [
        "Kotlin",
        "kot1in",
        "font_pack",
        "fontpack",
        "r0se-pine",
        "c-pp",
    ] {
        let err = match Spec::parse(fake) {
            Ok(spec) => match s.install(&spec, When::Always, &mut |_| {}) {
                Job::Failed { reason, .. } => reason,
                other => panic!("{fake}: {other}"),
            },
            Err(e) => e,
        };
        assert!(
            err.contains("did you mean") || err.contains("lower-case"),
            "{fake}: {err}"
        );
    }
    assert_eq!(
        lookalike_of("kot1in", v).map(|e| e.name.as_str()),
        Some("kotlin")
    );
    assert_eq!(lookalike_of("kotlin", v), None);
}

#[test]
fn specs_parse() {
    assert_eq!(Spec::parse("kotlin").unwrap(), Spec::Name("kotlin".into()));
    assert_eq!(
        Spec::parse("me/repo/pkg@v1.2").unwrap(),
        Spec::GitHub {
            owner: "me".into(),
            repo: "repo".into(),
            package: Some("pkg".into()),
            tag: Some("v1.2".into())
        }
    );
    assert_eq!(Spec::parse("./x").unwrap(), Spec::Dir("./x".into()));
    assert!(Spec::parse("me/../x").is_err());
    assert!(Spec::parse("a/b/c/d").is_err());
}

#[test]
fn a_project_asks_for_the_kits_its_files_need() {
    let root = temp("project");
    let p = root.join("app");
    write(&p.join("src/Main.kt"), "fun main() {}");
    write(&p.join("src/Util.kt"), "");
    write(&p.join("build.gradle.kts"), "");
    write(&p.join("Makefile"), "all:");
    // Ignored folders do not count.
    write(&p.join(".gitignore"), "vendor/\n");
    write(&p.join("vendor/x.rb"), "");
    std::fs::create_dir_all(p.join(".git")).unwrap();
    let s = store(&root);
    let plan = s.project_plan(&p).unwrap();
    let names: Vec<String> = plan.iter().map(|(s, _)| s.to_string()).collect();
    assert_eq!(names, ["kotlin", "make"]);
    assert_eq!(plan[0].1, "3 files (.kt, .kts)");

    // A project can say exactly what it needs.
    write(
        &p.join(".skink/packages.json"),
        r#"{"packages": ["font-pack", "me/x"]}"#,
    );
    let plan = s.project_plan(&p).unwrap();
    let names: Vec<String> = plan.iter().map(|(s, _)| s.to_string()).collect();
    assert_eq!(names, ["font-pack", "me/x"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_index_is_read_and_its_look_alikes_never_resolve() {
    if !update::have("curl") {
        return;
    }
    let root = temp("index");
    let index = r#"{"schema": 1, "packages": [
        {"name": "kot1in", "repo": "evil/kotlin", "verified": false},
        {"name": "fresh", "repo": "good/fresh", "verified": true, "extensions": ["fr"]},
        {"name": "Bad Name", "repo": "x/y"}
    ]}"#;
    let base = fake_http(vec![("/index.json".into(), 200, index.as_bytes().to_vec())]);
    let mut s = store(&root);
    s.index_url = format!("{base}/index.json");
    let idx = s.index().unwrap();
    assert_eq!(
        idx.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["kot1in", "fresh"]
    );
    assert_eq!(s.resolve("fresh").unwrap().repo, "good/fresh");
    assert!(
        s.resolve("kot1in")
            .unwrap_err()
            .contains("did you mean kotlin")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn broken_grammars_symlinks_clashes_and_long_versions_are_refused_at_install() {
    let root = temp("hostile");
    let s = store(&root);
    let fails = |dir: &Path, want: &str| {
        let job = s.install(&Spec::Dir(dir.to_path_buf()), When::Always, &mut |_| {});
        assert!(
            matches!(&job, Job::Failed { reason, .. } if reason.contains(want)),
            "{want}: {job}"
        );
    };
    // A cut-off module and a query the grammar cannot compile used to
    // install fine and leave files uncoloured with no word why.
    let cut = root.join("cut");
    package(&cut, "1", "");
    write(&cut.join("grammars/skinktest.wasm"), &wasm()[..1000]);
    fails(&cut, "grammar skinktest");
    let badq = root.join("badq");
    package(&badq, "1", "");
    write(&badq.join("queries/highlights.scm"), "((((( (nope) @x");
    fails(&badq, "highlights");
    // A symlink was skipped by the copy: "installed", then broken.
    #[cfg(unix)]
    {
        let link = root.join("link");
        package(&link, "1", "");
        std::fs::remove_file(link.join("themes/pkgtheme.json")).unwrap();
        std::os::unix::fs::symlink(cut.join(MANIFEST), link.join("themes/pkgtheme.json")).unwrap();
        fails(&link, "symlinks are not allowed");
    }
    let long = root.join("long");
    package(&long, &"9".repeat(500), "");
    fails(&long, "package.version");
    // Two packages may not claim one grammar name: the later replaced the
    // first one's grammar, and uninstalling either broke both.
    let ok = root.join("ok");
    package(&ok, "1", "");
    done(&s.install(&Spec::Dir(ok), When::Always, &mut |_| {}));
    let twin = root.join("twin");
    package(&twin, "1", "");
    let m = std::fs::read_to_string(twin.join(MANIFEST)).unwrap();
    write(
        &twin.join(MANIFEST),
        m.replacen("name = \"skinktest\"", "name = \"twin\"", 1),
    );
    fails(&twin, "already comes with package skinktest");
    assert_eq!(s.installed().len(), 1, "nothing half-installed");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_wrongly_cased_name_suggests_the_right_one() {
    let e = Spec::parse("Kotlin").unwrap_err();
    assert!(e.contains("did you mean kotlin?"), "{e}");
    let e = Spec::parse("font_pack").unwrap_err();
    assert!(e.contains("did you mean font-pack?"), "{e}");
}

#[test]
fn a_damaged_or_crafted_record_never_loses_or_deletes_the_wrong_thing() {
    let root = temp("db");
    let src = root.join("src");
    package(&src, "0.1.0", "");
    let s = store(&root);
    done(&s.install(&Spec::Dir(src), When::Always, &mut |_| {}));
    // Garbage used to read as "nothing installed"; the next save forgot it.
    write(&s.data.join(DB), "\0{{{");
    assert_eq!(s.installed()[0].name, "skinktest");
    // Entry names are paths: "" and ".." are dropped, never removed.
    let bait = root.join("bait");
    write(&bait.join("keep"), "x");
    write(
        &s.data.join(DB),
        r#"{"packages": {"": {}, "../../bait": {}, "skinktest": {"version": "0.1.0"}}}"#,
    );
    assert_eq!(s.installed().len(), 1);
    assert!(s.uninstall("../../bait").is_err() && s.uninstall("").is_err());
    assert!(bait.join("keep").is_file());
    // A record that cannot be written leaves the package in place.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&s.data, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = s.uninstall("skinktest");
        std::fs::set_permissions(&s.data, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(r.is_err());
        assert!(s.pkg_dir("skinktest").join(MANIFEST).is_file());
    }
    s.uninstall("skinktest").unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
