//! Shell integration: OSC 133 prompt marks for the shells Skink starts, so
//! prompt jumps, "select last output" and "send the last errors" are exact.
//!
//! The approach (not the code) follows VS Code's and Ghostty's shell
//! integration: start the shell with a small init file of ours that first
//! loads the user's own rc files unchanged, then adds hooks. The user's rc
//! files are never edited. The scripts here are written for Skink.
//!
//! Marks: `A` prompt starts, `B` prompt ends (input starts), `C` the command
//! runs (output starts), `D;<status>` it finished.

use std::path::{Path, PathBuf};

use crate::CommandSpec;

/// What [`integrate`] did for one terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integration {
    /// Our init file is loaded (the shell kind it was made for).
    Injected(Shell),
    /// The shell marks prompts itself (fish 4 and later).
    Native(Shell),
    /// Not a shell we know how to hook (cmd, nu, a program).
    Unsupported(String),
    /// `"terminal.shellintegration": false`, or the init files could not
    /// be written.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Pwsh,
}

impl Shell {
    /// The shell a program name stands for.
    pub fn of(program: &str) -> Option<Self> {
        let name = Path::new(program)
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        Some(match name.as_str() {
            "bash" => Shell::Bash,
            "zsh" => Shell::Zsh,
            "fish" => Shell::Fish,
            "pwsh" | "powershell" => Shell::Pwsh,
            _ => return None,
        })
    }
}

const BASH: &str = r#"# Skink shell integration for bash (loaded with --rcfile; your ~/.bashrc
# runs first, unchanged).
if [ -z "$__SKINK_SI_LOADED" ]; then
__SKINK_SI_LOADED=1
[ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"
__skink_si_status=0
__skink_si_ran=0
# 1 until the first prompt is drawn, so the rest of start-up is not marked.
__skink_si_inprompt=1
__skink_si_pre() {
    __skink_si_status=$?
    __skink_si_inprompt=1
    if [ "$__skink_si_ran" = 1 ]; then
        printf '\033]133;D;%s\007' "$__skink_si_status"
    fi
    __skink_si_ran=0
    printf '\033]133;A\007'
}
__skink_si_post() {
    case "$PS1" in
        *'133;B'*) ;;
        *) PS1="$PS1"'\[\033]133;B\007\]' ;;
    esac
    __skink_si_inprompt=0
    return $__skink_si_status
}
if [ -z "$__SKINK_SI_FORCE_DEBUG_TRAP" ] && { [ "${BASH_VERSINFO[0]}" -gt 4 ] ||
    { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; }; then
    # PS0 (bash 4.4+) prints before each command runs; the subscript sets
    # the flag in this shell (a $(...) would run in a subshell).
    PS0='\[\033]133;C\007\]${__skink_si_none[__skink_si_ran=1]}'"${PS0:-}"
elif [ -z "$(trap -p DEBUG)" ]; then
    # Older bash (macOS ships 3.2): a DEBUG trap marks the first command of
    # each line, never the prompt's own commands or completion. A DEBUG trap
    # the user already set is left alone (then only prompts are marked).
    __skink_si_debug() {
        [ "$__skink_si_inprompt" = 1 ] && return
        [ "$__skink_si_ran" = 1 ] && return
        [ -n "$COMP_LINE" ] && return
        case "$BASH_COMMAND" in __skink_si_*) return ;; esac
        __skink_si_ran=1
        printf '\033]133;C\007'
    }
    trap '__skink_si_debug' DEBUG
fi
if [ -n "$PROMPT_COMMAND" ]; then
    PROMPT_COMMAND="__skink_si_pre;${PROMPT_COMMAND%;};__skink_si_post"
else
    PROMPT_COMMAND="__skink_si_pre;__skink_si_post"
fi
fi
"#;

/// `.zshenv`, `.zprofile`, `.zshrc` and `.zlogin` of the ZDOTDIR shim. Each
/// loads the user's file of the same name from their real ZDOTDIR (or
/// $HOME); `.zshrc` then hands ZDOTDIR back and adds the hooks.
const ZSHENV: &str = r#"# Skink shell integration for zsh: load your .zshenv, unchanged.
__skink_si_zdotdir="$ZDOTDIR"
ZDOTDIR="${SKINK_USER_ZDOTDIR:-$HOME}"
[ -r "$ZDOTDIR/.zshenv" ] && . "$ZDOTDIR/.zshenv"
# Your .zshenv may move ZDOTDIR; remember where, then come back to ours.
SKINK_USER_ZDOTDIR="$ZDOTDIR"
ZDOTDIR="$__skink_si_zdotdir"
"#;

const ZPROFILE: &str = r#"[ -r "$SKINK_USER_ZDOTDIR/.zprofile" ] && . "$SKINK_USER_ZDOTDIR/.zprofile"
"#;

const ZSHRC: &str = r#"# Skink shell integration for zsh: your .zshrc (oh-my-zsh and all) runs
# first, unchanged; then ZDOTDIR is yours again for .zlogin and later.
ZDOTDIR="$SKINK_USER_ZDOTDIR"
unset __skink_si_zdotdir
[ -r "$ZDOTDIR/.zshrc" ] && . "$ZDOTDIR/.zshrc"
__skink_si_ran=0
__skink_si_precmd_first() {
    local s=$?
    if [[ $__skink_si_ran == 1 ]]; then
        printf '\033]133;D;%s\007' "$s"
    fi
    __skink_si_ran=0
    printf '\033]133;A\007'
    return $s
}
__skink_si_precmd_last() {
    # Themes may rebuild the prompt in their own precmd; mark its end again.
    [[ $PS1 == *'133;B'* ]] || PS1="$PS1"$'%{\033]133;B\007%}'
}
__skink_si_preexec() {
    [[ $__skink_si_ran == 1 ]] && return
    __skink_si_ran=1
    printf '\033]133;C\007'
}
if [[ -z $__skink_si_loaded ]]; then
    __skink_si_loaded=1
    precmd_functions=(__skink_si_precmd_first $precmd_functions __skink_si_precmd_last)
    preexec_functions+=(__skink_si_preexec)
fi
"#;

const ZLOGIN: &str = r#"[ -r "$SKINK_USER_ZDOTDIR/.zlogin" ] && . "$SKINK_USER_ZDOTDIR/.zlogin"
"#;

const FISH: &str = r#"function __skink_si_prompt --on-event fish_prompt; printf '\e]133;A\a'; end
function __skink_si_preexec --on-event fish_preexec; printf '\e]133;C\a'; end
function __skink_si_postexec --on-event fish_postexec; printf '\e]133;D;%s\a' $status; end
"#;

const PWSH: &str = r#"# Skink shell integration for PowerShell (your profile has already run).
if (-not $Global:__SkinkSiLoaded) {
    $Global:__SkinkSiLoaded = $true
    $Global:__SkinkSiRan = $false
    $Global:__SkinkSiPrompt = $function:prompt
    function Global:prompt {
        $ok = $?
        $code = if ($ok) { 0 } elseif ($LASTEXITCODE) { $LASTEXITCODE } else { 1 }
        $out = ""
        if ($Global:__SkinkSiRan) { $out += "$([char]27)]133;D;$code$([char]7)" }
        $Global:__SkinkSiRan = $false
        $out += "$([char]27)]133;A$([char]7)"
        $out += (& $Global:__SkinkSiPrompt)
        $out + "$([char]27)]133;B$([char]7)"
    }
    if (Get-Command PSConsoleHostReadLine -ErrorAction SilentlyContinue) {
        $Global:__SkinkSiReadLine = $function:PSConsoleHostReadLine
        function Global:PSConsoleHostReadLine {
            $line = & $Global:__SkinkSiReadLine
            $Global:__SkinkSiRan = $true
            [Console]::Write("$([char]27)]133;C$([char]7)")
            $line
        }
    }
}
"#;

/// Write `text` to `path` unless it already holds exactly that.
fn put(path: &Path, text: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|t| t == text) {
        return Ok(());
    }
    std::fs::write(path, text)
}

/// fish 4 and later emit OSC 133 themselves.
fn fish_is_native(program: &str) -> bool {
    use std::sync::OnceLock;
    static NATIVE: OnceLock<bool> = OnceLock::new();
    *NATIVE.get_or_init(|| {
        std::process::Command::new(program)
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| {
                let v = String::from_utf8_lossy(&o.stdout).into_owned();
                let num = v
                    .rsplit(' ')
                    .next()?
                    .trim()
                    .split('.')
                    .next()?
                    .parse::<u32>()
                    .ok()?;
                Some(num >= 4)
            })
            .unwrap_or(false)
    })
}

/// Hook `spec` (an interactive shell with no arguments) up to shell
/// integration, writing the init files under `dir`. Anything else is
/// returned unchanged.
pub fn integrate(spec: CommandSpec, dir: &Path) -> (CommandSpec, Integration) {
    let Some(shell) = Shell::of(&spec.program) else {
        let name = spec.program.clone();
        return (spec, Integration::Unsupported(name));
    };
    if !spec.args.is_empty() {
        let name = spec.program.clone();
        return (spec, Integration::Unsupported(name));
    }
    let made = |files: &[(&str, &str)]| -> Option<PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        for (name, text) in files {
            put(&dir.join(name), text).ok()?;
        }
        Some(dir.to_path_buf())
    };
    match shell {
        Shell::Bash => match made(&[("skink.bash", BASH)]) {
            Some(d) => {
                let rc = d.join("skink.bash").to_string_lossy().into_owned();
                (
                    spec.args(["--rcfile", rc.as_str()]),
                    Integration::Injected(shell),
                )
            }
            None => (spec, Integration::Off),
        },
        Shell::Zsh => {
            let zdir = dir.join("zsh");
            let files = [
                (".zshenv", ZSHENV),
                (".zprofile", ZPROFILE),
                (".zshrc", ZSHRC),
                (".zlogin", ZLOGIN),
            ];
            let ok = std::fs::create_dir_all(&zdir).is_ok()
                && files.iter().all(|(n, t)| put(&zdir.join(n), t).is_ok());
            if !ok {
                return (spec, Integration::Off);
            }
            // The user's own ZDOTDIR (if any) is where their files really are;
            // the spec's environment wins over ours.
            let var = |k: &str| {
                spec.env
                    .iter()
                    .rev()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.clone())
                    .or_else(|| std::env::var(k).ok())
                    .filter(|v| !v.is_empty())
            };
            let user = var("ZDOTDIR").or_else(|| var("HOME")).unwrap_or_default();
            let spec = spec
                .env("SKINK_USER_ZDOTDIR", user)
                .env("ZDOTDIR", zdir.to_string_lossy().into_owned());
            (spec, Integration::Injected(shell))
        }
        Shell::Fish if fish_is_native(&spec.program) => (spec, Integration::Native(shell)),
        Shell::Fish => (
            spec.args(["--init-command", FISH]),
            Integration::Injected(shell),
        ),
        Shell::Pwsh => match made(&[("skink.ps1", PWSH)]) {
            Some(d) => {
                let ps1 = d.join("skink.ps1").to_string_lossy().into_owned();
                let cmd = format!(". '{}'", ps1.replace('\'', "''"));
                (
                    spec.args(["-NoExit", "-Command", cmd.as_str()]),
                    Integration::Injected(shell),
                )
            }
            None => (spec, Integration::Off),
        },
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{MarkKind, Term};
    use std::time::Duration;

    /// A temp HOME with rc files that set an alias, and a dir for our files.
    fn home(tag: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("skink-si-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        std::fs::write(
            h.join(".bashrc"),
            "alias hi='echo HELLO_FROM_RC'\nPS1='$ '\n",
        )
        .unwrap();
        std::fs::write(
            h.join(".zshrc"),
            "alias hi='echo HELLO_FROM_RC'\nPS1='%% '\nprecmd() { true }\n",
        )
        .unwrap();
        h
    }

    /// Run `hi` then `false` in the integrated shell; the alias works and
    /// the marks frame the output with the right exit codes.
    fn check(program: &str, tag: &str) {
        check_env(program, tag, &[]);
    }

    fn check_env(program: &str, tag: &str, env: &[(&str, &str)]) {
        let on_path = std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()));
        if !on_path {
            return;
        }
        let h = home(tag);
        let spec = CommandSpec::new(program)
            .cwd(&h)
            .env("HOME", h.to_string_lossy().into_owned())
            .env("ZDOTDIR", h.to_string_lossy().into_owned());
        let spec = env
            .iter()
            .fold(spec, |sp, (k, v)| sp.env(*k, (*v).to_owned()));
        let (spec, how) = integrate(spec, &h.join("si"));
        assert_eq!(how, Integration::Injected(Shell::of(program).unwrap()));
        let t = Term::spawn(spec, 24, 80, 1000).unwrap();
        let marks_have = |t: &Term, n: usize| {
            t.marks()
                .iter()
                .filter(|m| matches!(m.kind, MarkKind::Done(_)))
                .count()
                >= n
        };
        assert!(
            t.wait_for(Duration::from_secs(10), |_| !t.marks().is_empty()),
            "{tag}: no prompt mark"
        );
        t.write_input(b"hi\r").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !marks_have(&t, 1) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        t.write_input(b"false\r").unwrap();
        while !marks_have(&t, 2) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let kinds: Vec<MarkKind> = t.marks().iter().map(|m| m.kind).collect();
        let done: Vec<_> = kinds
            .iter()
            .filter_map(|k| match k {
                MarkKind::Done(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(done, [Some(0), Some(1)], "{tag}: {kinds:?}");
        // One prompt, input and output mark per command, in order.
        use MarkKind::*;
        assert_eq!(
            &kinds[..10],
            [
                Prompt,
                Input,
                Output,
                Done(Some(0)),
                Prompt,
                Input,
                Output,
                Done(Some(1)),
                Prompt,
                Input
            ],
            "{tag}"
        );
        let text: String = t
            .history()
            .rows
            .iter()
            .map(|(r, _)| r.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("HELLO_FROM_RC"),
            "{tag}: the user's alias did not load:\n{text}"
        );
        let _ = t.kill();
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn bash_marks_prompts_and_keeps_the_users_rc() {
        check("bash", "bash");
    }

    /// macOS ships bash 3.2 (no PS0): the DEBUG-trap path must give the same
    /// marks. Forced here on a newer bash; CI on macOS runs it for real.
    #[test]
    fn old_bash_marks_commands_through_the_debug_trap() {
        check_env(
            "bash",
            "bash-debugtrap",
            &[("__SKINK_SI_FORCE_DEBUG_TRAP", "1")],
        );
    }

    #[test]
    fn zsh_marks_prompts_and_keeps_the_users_rc() {
        check("zsh", "zsh");
    }

    #[test]
    fn programs_and_shells_with_arguments_are_left_alone() {
        let d = std::env::temp_dir();
        let (s, how) = integrate(CommandSpec::new("htop"), &d);
        assert_eq!(
            (s.args.len(), how),
            (0, Integration::Unsupported("htop".into()))
        );
        let (s, how) = integrate(CommandSpec::new("bash").args(["-c", "x"]), &d);
        assert_eq!(
            (s.args.len(), how),
            (2, Integration::Unsupported("bash".into()))
        );
    }
}
