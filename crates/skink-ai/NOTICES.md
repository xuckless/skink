# Third-party notices for `skink-ai`

## Copied / adapted code and data

### Agent Client Protocol schema (wire field names)
- Project: agentclientprotocol/agent-client-protocol, crate `agent-client-protocol-schema` v1.15.1
  (<https://github.com/agentclientprotocol/agent-client-protocol>), Apache-2.0, Copyright (c) Zed Industries.
- Files: `src/v1/{agent,client,content,tool_call,plan}.rs` of the crate (read from the crates.io source).
- Used in: `src/acp/wire.rs` (method names, JSON field names, enum spellings, error codes).
- Changes: hand-written minimal serde subset (no `schemars`, `serde_with`, `derive_more`, `anyhow`, no
  `unstable_*` features), lenient deserialisation (unknown enum values and extra fields tolerated).
  The crate itself was not used because its dependency tree (about 50 crates) is far heavier than the subset needs.

### Lapce JSON-RPC stdio transport (framing idea, re-written)
- Project: lapce/lapce, `lapce-rpc/src/stdio.rs` @ f66ffaa95387794205c35864517de70958b9ea8d,
  Apache-2.0, Copyright (c) Lapce Authors.
- Used in: `src/acp/client.rs` (header comment). One JSON value per line, serialise-then-flush writes, a reader
  thread that classifies request / notification / response.
- Changes: new implementation; the reader survives malformed lines, invalid UTF-8 and oversized lines, accepts
  string ids, fails all pending calls on EOF, no crossbeam/anyhow/tracing.

## Dependencies (crates.io)
- `serde`, `serde_json` (MIT OR Apache-2.0).
- `similar` 3.x (Apache-2.0, Copyright (c) Armin Ronacher): line diffs for `prompter::edit`. Features: `text` only, no
  transitive dependencies.

## Ideas only (no code copied)
- ThePrimeagen/99 (no license; README/docs concepts only): `@file` / `#rule` context, AGENTS.md walk-up,
  rewrite-selection with inline diff, search results list, worker that keeps prompting.
  Implemented in `src/prompter/`. Template wording and extraction heuristics are original.
- Zed's ACP client (GPL): not read for code. The list of well-known agent command lines in `agent::known_agents`
  is public information from the ACP agent registry / each project's README.
