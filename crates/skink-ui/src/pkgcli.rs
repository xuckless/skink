//! `skink --install / --uninstall / --list / --search / --update-packages`.

use crate::packages::{self, Job, PkgState, Spec, Store, When};
use std::io::{IsTerminal, Write};

pub const USAGE: &str = "  skink --install NAME|owner/repo[/package][@tag]|DIR|. [-y]
  skink --uninstall NAME | --list | --search TEXT | --update-packages";

/// Whether `args` is a package command.
pub fn wants(args: &[String]) -> bool {
    args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--install" | "--uninstall" | "--list" | "--search" | "--update-packages"
        )
    })
}

/// Run a package command; the process exit code.
pub fn main(args: &[String]) -> u8 {
    let Some(store) = Store::open() else {
        eprintln!("skink: no data folder (set SKINK_DATA_DIR)");
        return 1;
    };
    let yes = args.iter().any(|a| a == "-y" || a == "--yes");
    let rest: Vec<&str> = args
        .iter()
        .skip(1)
        .map(String::as_str)
        .filter(|a| *a != "-y" && *a != "--yes")
        .collect();
    match args.first().map(String::as_str).unwrap_or_default() {
        "--install" if rest.is_empty() => usage("--install needs a package"),
        "--install" if rest == ["."] => install_project(&store, yes),
        "--install" => {
            let mut specs = Vec::new();
            for a in rest {
                match Spec::parse(a) {
                    Ok(s) => specs.push(s),
                    Err(e) => return usage(&e),
                }
            }
            install_all(&store, &specs, When::Always)
        }
        "--uninstall" if rest.is_empty() => usage("--uninstall needs a package"),
        "--uninstall" => {
            let mut code = 0;
            for n in rest {
                match store.uninstall(n) {
                    Ok(()) => println!("uninstalled {n}"),
                    Err(e) => {
                        eprintln!("skink: {e}");
                        code = 1;
                    }
                }
            }
            code
        }
        "--list" => {
            let all = store.installed();
            if all.is_empty() {
                println!(
                    "no packages installed (skink --install NAME, or --install . in a project)"
                );
            }
            for p in all {
                let state = match p.state {
                    PkgState::Installed => "on".to_owned(),
                    PkgState::Disabled => "off".to_owned(),
                    PkgState::Broken(why) => format!("broken: {why}"),
                };
                println!("{:<14} {:<9} {:<4} {}", p.name, p.version, state, p.source);
            }
            0
        }
        "--search" => search(&store, &rest.join(" ")),
        "--update-packages" => {
            let specs: Vec<Spec> = store
                .installed()
                .into_iter()
                .filter_map(|p| Spec::parse(&p.source).ok())
                .collect();
            if specs.is_empty() {
                println!("no packages installed");
                return 0;
            }
            install_all(&store, &specs, When::IfChanged)
        }
        other => usage(&format!("{other} goes first: skink --install NAME")),
    }
}

fn usage(msg: &str) -> u8 {
    eprintln!("skink: {msg}\nusage:\n{USAGE}");
    2
}

fn install_all(store: &Store, specs: &[Spec], when: When) -> u8 {
    let mut code = 0;
    for spec in specs {
        let label = spec.to_string();
        let job = store.install(spec, when, &mut |j| {
            if !matches!(j, Job::Done(_) | Job::Failed { .. }) {
                eprintln!("{label}: {j}");
            }
        });
        match &job {
            Job::Done(d) => {
                println!("{job}");
                for n in &d.notes {
                    println!("  note: {n}");
                }
            }
            _ => {
                eprintln!("skink: {label}: {job}");
                code = 1;
            }
        }
    }
    code
}

fn install_project(store: &Store, yes: bool) -> u8 {
    let root = std::env::current_dir().unwrap_or_default();
    let plan = match store.project_plan(&root) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("skink: {e}");
            return 1;
        }
    };
    if plan.is_empty() {
        println!("this project needs no packages that are not installed");
        return 0;
    }
    println!("This project would use:");
    for (spec, why) in &plan {
        println!("  {spec:<14} {why}");
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            eprintln!("skink: not a terminal; re-run with --yes to install these");
            return 1;
        }
        print!("Install them? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("nothing installed");
            return 0;
        }
    }
    let specs: Vec<Spec> = plan.into_iter().map(|(s, _)| s).collect();
    install_all(store, &specs, When::Always)
}

fn search(store: &Store, query: &str) -> u8 {
    let entries = match store.index() {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "skink: could not read the package index ({}): {e}",
                store.index_url
            );
            return 1;
        }
    };
    let q = query.to_ascii_lowercase();
    let verified: Vec<packages::Entry> = packages::verified()
        .iter()
        .cloned()
        .chain(entries.iter().filter(|e| e.verified).cloned())
        .collect();
    let mut shown = 0;
    for e in &entries {
        let hay = format!("{} {} {}", e.name, e.repo, e.description).to_ascii_lowercase();
        if !q.is_empty() && !hay.contains(&q) {
            continue;
        }
        shown += 1;
        let how = if e.verified {
            format!("verified   skink --install {}", e.name)
        } else {
            let install = match &e.package {
                Some(p) => format!("{}/{p}", e.repo),
                None => e.repo.clone(),
            };
            match packages::lookalike_of(&e.name, &verified) {
                Some(v) => format!(
                    "UNVERIFIED, looks like {}: skink --install {install}",
                    v.name
                ),
                None => format!("unverified skink --install {install}"),
            }
        };
        println!(
            "{:<14} {:<9} {}\n               {}",
            e.name, e.version, e.description, how
        );
    }
    if shown == 0 {
        println!("nothing found for {query:?}");
    }
    0
}
