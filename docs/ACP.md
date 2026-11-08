# ACP in Skink (`skink-ai`)

Skink is an [Agent Client Protocol](https://agentclientprotocol.com) **client**. It talks newline-delimited JSON-RPC
2.0 (protocol version 1) over any `(Read, Write)` pair to an agent such as `claude-code-acp`,
`gemini --experimental-acp`, `agy acp` or `goose acp`. No async runtime: one reader thread per agent plus short-lived
helper threads for the agent's `fs/*` and `terminal/*` requests.

## Layers

| Layer | Module | Purpose |
|---|---|---|
| Wire subset | `acp::wire` | Hand-written serde types, field names from the official schema (Apache-2.0). |
| Transport | `acp::transport` | `Transport { reader, writer, process }`, `spawn(AgentSpec)`, `memory_pipe()`. |
| Client | `acp::AcpClient` | `start`, `new_session`, `load_session`, `prompt` -> `TurnHandle`, `cancel`, `authenticate`, `set_mode`. |
| Client-side callbacks | `acp::AcpHandler`, `WorkspaceFs`, `TerminalHost` | Answer the agent's `fs/*` / `terminal/*` requests. |
| Editor seam | `AgentController` + `LocalAgents` | Object-safe, non-blocking, ACP-free; EveryAI's `Delegated` impl uses the same types. |
| Prompter | `prompter::*` | 99-style, UI-free: context, rewrite / search / worker, diffs. |

## Supported subset (protocol v1)

Client -> agent:

- `initialize` (`protocolVersion: 1`, `clientCapabilities.fs.{readTextFile,writeTextFile}` taken from the handler,
  `terminal` true only if the handler provides a `TerminalHost`, `clientInfo`). A different version in the reply is an
  error (`AcpError::Protocol`). The agent's `authMethods`, `loadSession` and `promptCapabilities.{image,embeddedContext}`
  are surfaced in `AgentInfo`.
- `authenticate { methodId }`. A `-32000` error on `session/new` sets `ErrorInfo::auth_required`.
- `session/new { cwd, mcpServers }` (stdio / http / sse MCP servers), `session/load` (only if `loadSession`),
  `session/set_mode`.
- `session/prompt` with `text`, `resource_link` and (if the agent advertises `embeddedContext`) `resource` blocks.
  Without that capability `PromptBlock::Resource` degrades to a fenced text block. One running turn per session;
  a second `prompt` returns `TurnInProgress`.
- `session/cancel` (notification). `cancel()` also answers all pending `session/request_permission` requests of that
  session with `{"outcome":"cancelled"}` first, as the spec requires; the turn then ends with `StopReason::Cancelled`.

Agent -> client:

- `session/update`: `agent_message_chunk`, `agent_thought_chunk`, `user_message_chunk` (history replay on load),
  `tool_call`, `tool_call_update`, `plan`, `available_commands_update`, `current_mode_update`. Other kinds
  (`usage_update`, `session_info_update`, ...) are ignored. Non-text chunks become short placeholders (`[image]`).
- `session/request_permission` -> `AgentEvent::Permission` carrying a one-shot `PermissionReply` (`select(option_id)` /
  `cancel()`). Dropping the reply unanswered sends `cancelled`, so the agent is never left hanging.
- `fs/read_text_file`, `fs/write_text_file` -> `AcpHandler`. `WorkspaceFs` restricts paths to the workspace root
  (lexical `..` removal plus symlink resolution of the deepest existing ancestor), serves **unsaved editor text** through
  `BufferSource::read`, and routes writes to open buffers through `BufferSource::write` (so the editor applies them as an
  undoable edit instead of changing disk under the user). Errors map to `-32002` (not found), `-32602` (outside the
  workspace), `-32603`.
- `terminal/create|output|wait_for_exit|kill|release` -> `AcpHandler::terminal()`; `None` (default) answers `-32601`.
  Implement `TerminalHost` (wire-free `TerminalSpec` / `TerminalOutput`) on top of `skink-term` when it lands.
- Unknown methods with an id get `-32601`; unknown notifications are ignored.

Not implemented: `session/fork|resume|close|status`, `logout`/`auth/status`, MCP-over-ACP, elicitation, unstable
extensions (`_meta` is ignored), batching.

## Robustness

Malformed JSON, non-object lines, blank lines, invalid UTF-8, string ids, unknown response ids and oversized lines
(> 32 MiB) produce `AgentEvent::ProtocolError` (capped at 50 per connection) and never stop the reader. Responses may
arrive in any order. If the agent exits, every pending call fails with `AcpError::Closed`, running turns emit
`TurnFailed`, pending permission replies are cancelled, the child is reaped and a final `AgentEvent::Exited { status,
stderr_tail }` is sent (last 50 stderr lines, to explain a crash).

## How the host launches agents

Standalone: `LocalAgents::start_agent(label, AgentSpec { program, args, cwd, env })` spawns the process with piped
stdio (`cwd` should be the workspace root) and handshakes on a background thread; poll
`AgentController::poll_events()` each UI tick (pass a waker with `LocalAgents::with_waker` to be notified). Common
command lines are in `skink_ai::known_agents()`:

| Agent | Command |
|---|---|
| Claude Code | `npx -y @zed-industries/claude-code-acp` |
| Gemini CLI | `gemini --experimental-acp` |
| Antigravity | `agy acp` |
| Goose | `goose acp` |
| Codex | `npx -y @zed-industries/codex-acp` |

Skink does not probe for installed agents; the host decides what to offer. On Windows `spawn` resolves `npx` to
`npx.cmd` via `PATH`.

Inside another host (terminal pane, EveryAI): run the process yourself, keep stdout/stdin as the transport and call
`LocalAgents::attach(label, Transport::new(stdout, stdin))` (or `AcpClient::start`). Anything `Read + Write` works:
a socket, a pty with raw mode and no echo, an in-process thread (`memory_pipe`). The agent's stdout must carry only
protocol lines (stray non-JSON output is reported as `ProtocolError`, not fatal); anything the agent prints for humans
belongs on stderr. For full EveryAI handover, implement `AgentController` against the host channel
(`backend() == Backend::Delegated`); no ACP wire types are involved.

Event flow for one prompt: `Ready` -> `new_session` -> `SessionStarted` -> `send_prompt` -> `MessageChunk`,
`ThoughtChunk`, `ToolCall`, `ToolCallUpdate`, `Plan`, `Permission` (answer it!) ... -> `TurnFinished { stop }` or
`TurnFailed`. Agents are killed (stdin closed, then `kill`) when `stop_agent` is called or `LocalAgents`/`AcpClient`
is dropped. Note that killing a wrapper such as `npx` may leave its Node child alive until it notices stdin closing.

## Prompter quick reference

```text
build_rewrite(cfg, files, &RewriteRequest, diagnostics) -> BuiltRewrite   // .prompt, .finish(reply) -> EditProposal
build_search(cfg, files, &SearchRequest)               -> BuiltSearch      // parse_search_results(reply, root, files)
Worker::start / on_reply / continue_prompt                                 // multi-round search tracker
EditProposal.diff: Diff { segments }   apply(|block| accepted) / accept_all / reject_all / unified(ctx)
EditProposal::apply_to(buffer, accept) // relocates the region if the buffer drifted, else Err(Stale)
TextIndex  // char (line, col) / absolute char index <-> byte offsets, CRLF aware, clamping, no panics
```

`@path[:a[-b]]` and `#rule` tokens in the user text are expanded (rules live in `.skink/rules/<name>.md` or
`.cursor/rules/`); `AGENTS.md` (else `AGENT.md`) is collected from the file's directory up to the project root, root
first. The agent is told not to edit files itself for rewrite/search: the editor applies the result, which keeps every
change reviewable.
