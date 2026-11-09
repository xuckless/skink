# EveryAI handover contract

When Skink runs inside EveryAI's terminal it becomes an MCP client of EveryAI's
local MCP host and can delegate LSP and agent work to it. This file is the spec
the EveryAI side implements. The Rust types are in
`crates/skink-host/src/tools.rs` (module `skink_host::tools`) and are the
source of truth for field names.

## 1. Detection

EveryAI sets two environment variables in the terminal it spawns Skink in:

| Variable | Example | Meaning |
|---|---|---|
| `SKINK_HOST_URL` | `http://127.0.0.1:41234/mcp` | The host's MCP endpoint (the same one agents get from `mint`). |
| `SKINK_HOST_TOKEN` | `eai_host_...` | The bearer token minted for this session. |

Both must be present and non-empty. If either is missing Skink runs standalone
(`Backend::Local`) and never contacts anything.

Skink calls `skink_host::connect_if_present()` once at start-up. It performs the
MCP handshake with short deadlines (300 ms connect, 1.5 s round trip). Only if
that succeeds is the backend `Delegated`. The host should therefore answer
`initialize` promptly.

Optionally the editor may call `tools/list` and enable only the features whose
tools are listed (a host may expose a subset; unlisted features stay local).

## 2. Transport (what Skink speaks)

MCP over Streamable HTTP, tools only, stateless (no `Mcp-Session-Id`), legacy
handshake:

* `POST {SKINK_HOST_URL}` with `Authorization: Bearer <token>`,
  `Content-Type: application/json`, `Accept: application/json, text/event-stream`,
  `Host: 127.0.0.1:<port>` (exactly; `localhost:<port>` / `[::1]:<port>` if the
  URL used those), `Connection: close`. One JSON-RPC 2.0 message per request.
* `initialize` with `protocolVersion: "2025-11-25"` (accepts a reply of
  `2025-11-25`, `2025-06-18` or `2025-03-26`), then
  `notifications/initialized` (expects `202`). Every later request carries
  `MCP-Protocol-Version: <negotiated>`.
* `tools/list` (follows `nextCursor`), `tools/call`.
* A `tools/call` reply is `text/event-stream`: `event: message\ndata: {json}\n\n`,
  `:` keep-alive comments, optional `notifications/progress` events (matched by
  the `progressToken` Skink put in `params._meta`), and the final JSON-RPC
  response, after which the connection closes. All other replies are plain JSON
  (`Content-Length` or `chunked`). Skink treats more than 120 s without any
  bytes (keep-alives count) as a dead host, so keep-alives must be sent.
* Cancel: Skink sends `notifications/cancelled` `{requestId, reason}` on a fresh
  connection (same token), then closes the call's socket. The host should stop
  the work for that request id.
* Errors: HTTP 401/403 = bad token / host; JSON-RPC errors for protocol
  problems; tool failures are results with `isError: true` and a plain text
  message.

## 3. Conventions for every tool

* `arguments` is a JSON object with `snake_case` fields as below.
* A successful result is **one `text` content block containing one JSON
  document** (the "Result" below). If the host also sets `structuredContent`,
  Skink prefers it. `isError: true` means the text is a human readable message
  (shown to the user, never parsed).
* Positions are LSP positions: zero-based `line`, `character` in UTF-16 code
  units. Ranges are `{start, end}` with end exclusive.
* Paths are absolute native file system paths (Windows: `C:\...`). The host
  maps to URIs itself.
* `doc.text`: when present it is the unsaved buffer content and must be treated
  as the document's current content (LSP `didChange`) before answering.
  `doc.version` is an optional monotonic buffer version.

Shared shapes:

```jsonc
Position  { "line": 0, "character": 0 }
Range     { "start": Position, "end": Position }
Location  { "path": "/abs/file.rs", "range": Range }
TextEdit  { "range": Range, "new_text": "..." }
FileEdit  { "path": "/abs/file.rs", "edits": [TextEdit] }
Doc       { "path": "/abs/file.rs", "text"?: "...", "version"?: 3 }
PositionArgs = Doc fields + { "line": 0, "character": 0 }   // flattened
```

## 4. LSP tools

All take the language server for the file's language that EveryAI manages for
the project. If none is available return `isError: true` with a message; Skink
then falls back to its own local LSP for that call.

### `lsp.hover`
Args: `PositionArgs`. Result: `null` or
`{ "contents": "<markdown>", "range"?: Range }`.

### `lsp.definition`
Args: `PositionArgs`. Result: `[Location]` (empty if none).

### `lsp.references`
Args: `PositionArgs + { "include_declaration"?: false }`. Result: `[Location]`.

### `lsp.diagnostics`
Args: `Doc`. Result:
`[{ "range": Range, "severity": "error"|"warning"|"info"|"hint", "message": "...", "code"?: "...", "source"?: "..." }]`.
Current diagnostics for the document (after applying `text` if given).

### `lsp.completion`
Args: `PositionArgs + { "trigger_character"?: "." }`. Result:
```jsonc
{ "is_incomplete": false,
  "items": [{ "label": "...", "kind"?: 3 /* LSP CompletionItemKind */,
              "detail"?: "...", "documentation"?: "<markdown>",
              "insert_text"?: "...", "text_edit"?: TextEdit,
              "sort_text"?: "...", "filter_text"?: "..." }] }
```
`text_edit` wins over `insert_text` when both are set.

### `lsp.rename`
Args: `PositionArgs + { "new_name": "..." }`. Result:
`{ "files": [FileEdit] }`: edits for every affected file. Skink shows these in
its refactor preview before applying; the host must **not** write files.

### `lsp.format`
Args: `Doc + { "range"?: Range, "tab_size"?: 4, "insert_spaces"?: true }`.
Result: `[TextEdit]` (empty if already formatted).

## 5. Prompt tools

### `prompt.run`
Rewrite a selection, search the project, or answer a question with EveryAI's AI.

Args:
```jsonc
{ "task_id": "client-generated unique id",
  "mode": "edit" | "search" | "ask",
  "instruction": "free text from the user",
  "file"?: Doc,                       // file being edited (with unsaved text)
  "selection"?: { "range": Range, "text": "..." },
  "context": [{ "kind": "file"|"rule"|"text",   // @path, #rule, literal
                "name": "src/lib.rs | rule-name | label",
                "text"?: "inline content; if absent the host resolves name" }],
  "model"?: "id from models.list",
  "cwd"?: "/abs/project/root" }
```
Streaming: while running, the host may send `notifications/progress` whose
`message` is a JSON string, one of
```jsonc
{ "type": "status",     "text": "reading files" }
{ "type": "text_delta", "text": "partial answer" }
{ "type": "edit",       "file": FileEdit }       // an edit as soon as it is known
{ "type": "location",   "location": Location }   // a search hit as soon as found
```
Skink uses these only for live display; messages that are not valid events are
ignored. `progress` should increase monotonically.

Result (the filled field matches `mode`; others omitted):
```jsonc
{ "task_id": "...",
  "files"?: [FileEdit],         // mode "edit"; edits are against the content Skink sent
  "locations"?: [Location],     // mode "search"
  "text"?: "..." }              // mode "ask" (markdown)
```
The host must not write files: Skink presents `files` as an inline diff to
accept or reject, multi-file edits through the refactor preview. Cancelling the
request (`notifications/cancelled` or socket close) must stop the work.

### `prompt.cancel`
Args: `{ "task_id": "..." }`. Result: `{ "cancelled": true|false }` (false if no
such running task). For cancelling from a different connection than the one
running `prompt.run`; the usual path is MCP request cancellation.

## 6. Models and agents

### `models.list`
Args: `{}`. Result:
`{ "models": [{ "id": "...", "name": "...", "provider"?: "...", "default": false }] }`.

### `agents.list`
Args: `{}`. Result:
`{ "agents": [{ "id": "claude-code", "name": "...", "description"?: "...", "installed": true, "running": false }] }`.
ACP-capable agents EveryAI knows about and manages.

### `agents.start`
Args: `{ "agent_id": "...", "cwd"?: "/abs/dir", "prompt"?: "first message", "model"?: "..." }`.
Result: `{ "session_id": "..." }`. EveryAI owns the ACP session and shows it in
its own UI; Skink only keeps the id.

## 7. Terminal (optional)

### `ui.font_size`
Args: `{ "step": "bigger" | "smaller" | "reset" }`. Result: `{ "size": 13.0 }`
(the new font size in points). Changes the font size of the terminal pane Skink
runs in; this is Skink's text zoom (`:fontsize +1|-1|0`, actions `FontBigger`,
`FontSmaller`, `FontReset`). Optional: if the host does not list it, Skink falls
back to Kitty remote control or tells the user to use the terminal's own zoom.

## 8. Security

* **Loopback only.** Skink refuses any `SKINK_HOST_URL` that is not plain
  `http://` on exactly `127.0.0.1`, `localhost` or `[::1]` (no `https`, no
  userinfo, no other hosts such as `127.0.0.1.evil.com`). `localhost` is
  additionally re-checked after resolution: only loopback addresses are used.
  The check happens before any socket is opened.
* **Token never logged.** It lives only in the env and in memory, is sent only
  in the `Authorization` header to that URL, and is redacted from every `Debug`
  / `Display` output and error message in `skink-host`. It is rejected if it
  contains characters that could inject headers. EveryAI should treat it as a
  per-session secret (the existing `eai_host_...` tokens already are) and not
  put it on command lines (use the environment).
* The host keeps its DNS-rebinding guard (exact `Host` header) and token check;
  Skink sends the exact `Host: 127.0.0.1:<port>`.
* Responses are size capped (16 MiB per body or SSE message; header lines 16 KiB,
  100 headers) and every socket read has a deadline.
* The host never writes to Skink's files through these tools; all changes come
  back as edits Skink applies (and the user can reject).

## 9. Fallbacks

Delegation must never make Skink worse than standalone.

* No env vars, non-loopback URL, connection refused, 401, timeout or a broken
  handshake at start-up: Skink stays standalone (`Backend::Local`), silently,
  with no error dialog.
* A delegated call that fails mid-session (timeout, host gone, `isError`,
  tool missing from `tools/list`): that feature falls back to the local
  implementation (local LSP / local AI route) and the status bar notes that the
  host is unavailable. Skink retries the connection lazily on the next request,
  not in a loop.
* A running call is bounded by an idle timeout (120 s without bytes). The user
  can always cancel; cancellation drops the socket even if the host is
  unreachable.
* Nothing in `skink-host` panics on I/O or malformed replies; all failures are
  `skink_host::Error` values.

## 10. Client API summary (for the Skink side)

```rust
let ep = HostEndpoint::from_env()?;                 // SKINK_HOST_URL + SKINK_HOST_TOKEN
let client = McpClient::connect(&ep)?;              // initialize + initialized
let tools = client.list_tools()?;
let r = client.call_tool("lsp.hover", &json!({..}))?;       // ToolResult{text,is_error,images,structured}
let h: HoverResult = client.call_typed(tools::LSP_HOVER, &args)?;
let cancel = client.cancel_handle();                // Clone + Send; cancel.cancel() from any thread
client.call_tool_progress(tools::PROMPT_RUN, &args, &mut |p| { /* PromptEvent::parse(p.message) */ })?;
```
