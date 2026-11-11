# Remote development

```sh
skink ssh user@host:~/app
skink ssh pi
```

Skink runs on the remote, next to the code: language servers, git, tasks and terminals all run there. Your ssh keys, agent and `~/.ssh/config` (aliases, jump hosts) work as usual; host-key and password prompts appear before the editor starts. One connection carries everything (OpenSSH ControlMaster; on Windows one connection per task).

The first time, or after an update, skink copies itself to `~/.skink/bin/skink` on the remote (Linux and macOS, x86_64 and arm64). It downloads here and copies over ssh, so the remote needs no internet.

## Ports

When a program in a terminal tab starts listening, its port is forwarded to 127.0.0.1 on your machine and a toast shows the URL; a taken port moves to the next free one. It stops when the program stops.

| Command | Does |
|---|---|
| `:ports` | what listens and where it is forwarded |
| `:port 3000` / `:port 3000:8080` | forward one by hand (remote:local) |
| `:ports stop PORT` | stop a forward |
| `:remotes` | the saved remotes |

## Settings

```json
"ssh.autoforward": true,
"remotes": {"pi": {"host": "pi@10.0.0.5", "path": "~/app",
                   "forwards": ["3000", "5432:15432"], "autoforward": true}}
```

`ssh.autoforward`: true forwards, `"ask"` offers `:port`, false only uses the remote's `forwards` and `:port`. Clipboard copies (OSC 52) reach your machine's clipboard through ssh.
