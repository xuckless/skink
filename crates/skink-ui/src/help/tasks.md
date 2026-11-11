# Tasks

| Key | Does |
|---|---|
| Shift-F10 | pick a task and run it in a terminal tab (`:task [LABEL]`) |
| Shift-F9 | run the last task again (`:task last`) |

Tasks come from `.skink/tasks.json` and `.vscode/tasks.json`, plus ones detected from Cargo.toml, package.json scripts (npm, pnpm, yarn, bun), go.mod, build.zig, pytest setups and Makefile targets.

```json
{"version": "2.0.0", "tasks": [
  {"label": "test", "command": "cargo", "args": ["test"],
   "options": {"cwd": "${workspaceFolder}"}, "problemMatcher": "$rustc"}
]}
```

The VS Code subset: label, command, args, options.cwd, options.env, group, problemMatcher and per-OS `linux` / `osx` / `windows` overrides. `${workspaceFolder}` `${file}` `${relativeFile}` `${fileDirname}` `${fileBasename}` `${env:NAME}` expand.

## Problems

When a task ends its output goes through problem matchers: `$rustc`, `$tsc`, `$eslint-stylish`, `$eslint-compact`, `$go`, `$gcc`, Python tracebacks and pytest. With no problemMatcher all are tried; `[]` turns them off. Problems show in the gutter, Shift-F8 and F6 like a language server's, until the next run.
