# Debugging

| Key | Does |
|---|---|
| F5 | start (the last configuration, else pick one), or continue |
| Shift-F5 | stop the program (twice: kill the adapter) |
| F10 / F11 / Shift-F11 | step over / into / out, while debugging |
| F6 | pause while it runs |
| F9 | breakpoint on the line (or click the sign column) |
| Alt-F9 | condition, hit count or logpoint |
| Alt-L | the debug panel |
| Alt-K | while paused: the value at the cursor |

Outside a session F10, F11, Shift-F11 and F6 keep their usual jobs.

While paused the stopped line is highlighted and local values show at the ends of the lines above. Breakpoints follow edits and are saved in the session with the watches.

## The panel

State, call stack (Enter shows a frame), variables (Enter / Space expands), watch (w adds, d removes), breakpoints (Enter jumps, d removes), console (e evaluates).

## Commands

```
:debug              pick a configuration
:debug NAME         start one
:debug restart      :debug pause    :debug panel
:debug eval EXPR    :debug watch EXPR
:breakpoint if i > 3
:breakpoint hits 5
:breakpoint log x={x}
:breakpoint clear   :breakpoint clearall
```

## Configurations

From `.skink/launch.json` and `.vscode/launch.json` (VS Code subset: name, type, request launch / attach, preLaunchTask, per-OS overrides; other fields go to the adapter with `${...}` expanded). Without one: each cargo binary, the Python file or pytest test at the cursor, the Go package or test at the cursor, and a native program built next to the file (`cc -g x.c -o x`).

| Adapter (on PATH) | For | type |
|---|---|---|
| gdb 14+ (`gdb -i dap`) | C, C++, Rust, Zig | gdb |
| lldb-dap | LLVM | lldb-dap |
| codelldb | CodeLLDB | lldb |
| debugpy (`pip install debugpy`) | Python | debugpy |
| dlv (`dlv dap`) | Go | go |

js-debug is not supported yet. Programs an adapter runs "in a terminal" open in a terminal tab.
